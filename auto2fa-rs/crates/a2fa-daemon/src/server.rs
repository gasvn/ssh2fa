//! Unix-socket server — the top-level daemon entry point.
//!
//! # Socket and lock paths
//!
//! | Path                         | Default                      | Override env var  |
//! |------------------------------|------------------------------|-------------------|
//! | IPC socket                   | `~/.auto2fa/auto2fa.sock`    | `AUTO2FA_SOCK`    |
//! | Singleton flock              | `~/.auto2fa/lock`            | `AUTO2FA_LOCK`    |
//!
//! Setting `AUTO2FA_SOCK` / `AUTO2FA_LOCK` lets you smoke-test the Rust daemon
//! against a temp directory without touching the paths used by a running Python
//! daemon.
//!
//! # Lifecycle
//!
//! 1. Acquire the exclusive flock — exit cleanly if another daemon holds it.
//! 2. makedirs `~/.auto2fa`.
//! 3. Remove any stale socket.
//! 4. Bind `UnixListener` at the socket path, chmod 0600.
//! 5. Load `State` (config + creds) from `passwords.json` + `tunnels.json`.
//! 6. Spawn `poll_loop` (tick thread).
//! 7. Accept loop: spawn a thread per connection.
//!    - Each thread reads newline-delimited JSON, calls `dispatch`, writes the
//!      response.
//!    - `subscribe_events` is intercepted before `dispatch` to wire up the
//!      mpsc fan-out channel.
//! 8. On SIGINT / SIGTERM: set the stop flag and join the tick thread.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use a2fa_core::config::{config_dir, passwords_path};
use a2fa_core::engine::{tick::poll_loop, State};
use a2fa_core::proto::{encode_response, Method};

use crate::dispatch::{dispatch_with_ctx, DaemonCtx};
use crate::managers::{boot_autostart, start_heartbeat, HostManagers};
use crate::singleton::acquire_lock;
use crate::subscribers;
use crate::tunnel_maintenance::start_tunnel_maintenance;
use crate::tunnel_runtime::TunnelRuntime;
use crate::workers::OtpRegistry;

// ---------------------------------------------------------------------------
// Path resolution helpers
// ---------------------------------------------------------------------------

fn resolve_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

/// Path to the Unix socket.
///
/// Overridable via `AUTO2FA_SOCK` for smoke-testing without disturbing the
/// running Python daemon.
pub fn socket_path() -> PathBuf {
    std::env::var("AUTO2FA_SOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|_| resolve_home().join(".auto2fa").join("auto2fa.sock"))
}

/// Path to the singleton lock file.
///
/// Overridable via `AUTO2FA_LOCK`.
pub fn lock_path() -> PathBuf {
    std::env::var("AUTO2FA_LOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|_| resolve_home().join(".auto2fa").join("lock"))
}

// ---------------------------------------------------------------------------
// run()
// ---------------------------------------------------------------------------

/// Start the daemon.  Blocks until SIGINT or SIGTERM.
pub fn run() -> Result<()> {
    let lock_p = lock_path();
    let sock_p = socket_path();

    log::info!("a2fa-daemon starting (sock={}, lock={})", sock_p.display(), lock_p.display());

    // 1. Acquire singleton flock.
    let _lock_file = match acquire_lock(&lock_p)? {
        Some(f) => f,
        None => {
            log::warn!(
                "another auto2fa daemon already holds {} — exiting",
                lock_p.display()
            );
            return Ok(());
        }
    };

    // 2. Ensure socket directory exists.
    if let Some(dir) = sock_p.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create socket dir {:?}", dir))?;
    }

    // 3. Remove stale socket.
    if sock_p.exists() {
        std::fs::remove_file(&sock_p)
            .with_context(|| format!("remove stale socket {:?}", sock_p))?;
    }

    // 4. Bind and chmod.
    let listener = UnixListener::bind(&sock_p)
        .with_context(|| format!("bind UnixListener at {:?}", sock_p))?;
    std::fs::set_permissions(&sock_p, std::fs::Permissions::from_mode(0o600))
        .context("chmod socket 0600")?;

    // 5. Load State.
    let cfg_dir = config_dir();
    let tunnels_path = cfg_dir.join("tunnels.json");
    let passwords_p = passwords_path();

    // 5-pre. Migrate passwords.json v1 → v2 (one-time, idempotent).
    //        Must run BEFORE State::new, which calls load_meta and silently
    //        returns zero hosts for any non-v2 file.
    {
        let kc = a2fa_core::creds::keychain::KeychainStore;
        match a2fa_core::creds::migrate::migrate_passwords_file_if_needed(&kc, &passwords_p) {
            Ok(true)  => log::info!("migrated passwords.json v1 -> v2 (creds moved to Keychain)"),
            Ok(false) => {}
            Err(e)    => log::error!("passwords.json migration failed (continuing): {e}"),
        }
    }

    let state = Arc::new(Mutex::new(State::new(tunnels_path, &passwords_p)));

    {
        let guard = state.lock().unwrap();
        log::info!(
            "state loaded: {} hosts, {} tunnels",
            guard.hosts.len(),
            guard.tunnels.len()
        );
    }

    // 5a. Reap stray ssh -L tunnel processes left from a previous daemon run.
    {
        let ports: Vec<u16> = state
            .lock()
            .unwrap()
            .tunnels
            .iter()
            .map(|t| t.local_port)
            .collect();
        let reaped = a2fa_core::tunnels::cleanup_orphans(&ports);
        log::info!("cleanup_orphans: reaped {reaped} stray tunnel process(es)");
    }

    // 5b. Create daemon-global persistent host managers + OTP registry.
    let managers = HostManagers::new();
    let registry = OtpRegistry::new();

    // 5c. Create the tunnel runtime registry (child handles + runtime counters).
    let runtime = TunnelRuntime::new();

    // Record daemon startup time so the maintenance loop can fire boot auto-start
    // after a 3-second grace period (mirrors Python's startup_ts + 3s logic).
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        runtime.set_startup_ts(now);
    }

    // 5d. Boot auto-start: connect every host that was active at last shutdown.
    //     This mirrors Python's `init_managers` which starts active-host threads
    //     on daemon startup.
    boot_autostart(&state, &managers, &registry);

    // 6. Spawn tick thread.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let state2 = Arc::clone(&state);
        let stop2 = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("tick-loop".into())
            .spawn(move || poll_loop(&state2, &stop2))
            .context("spawn tick thread")?;
    }

    // 6b. Spawn heartbeat / auto-reconnect thread.
    //     Loops every ~3 s, heartbeats each active host's pool slots, and
    //     restarts dead masters — the Rust port of `manage_pool_loop`.
    start_heartbeat(Arc::clone(&state), Arc::clone(&managers), Arc::clone(&registry));

    // 6c. Spawn tunnel maintenance thread.
    //     Runs every ~1 s: auto-recovery, child-died detection, squeue/stale,
    //     and boot auto-start — the Rust port of `TunnelManager.tick()`.
    {
        use std::collections::HashSet;
        let post_connect_running: Arc<std::sync::Mutex<HashSet<String>>> =
            Arc::new(std::sync::Mutex::new(HashSet::new()));
        start_tunnel_maintenance(
            Arc::clone(&state),
            Arc::clone(&runtime),
            post_connect_running,
        );
    }

    log::info!("daemon listening on {}", sock_p.display());
    println!("a2fa-daemon listening on {}", sock_p.display());

    // Build the shared daemon context (cloned cheaply per connection).
    let ctx = DaemonCtx {
        state: Arc::clone(&state),
        managers: Arc::clone(&managers),
        registry: Arc::clone(&registry),
        runtime: Arc::clone(&runtime),
    };

    // 7. Accept loop.
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let ctx2 = ctx.clone();
                std::thread::Builder::new()
                    .name("conn".into())
                    .spawn(move || handle_connection(stream, ctx2))
                    .context("spawn connection thread")?;
            }
            Err(e) => {
                log::error!("accept error: {e}");
                break;
            }
        }
    }

    // 8. Teardown.
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&sock_p);
    log::info!("daemon stopped");
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-connection handler
// ---------------------------------------------------------------------------

fn handle_connection(stream: UnixStream, ctx: DaemonCtx) {
    log::debug!("client connected");

    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            log::error!("stream clone failed: {e}");
            return;
        }
    };

    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;

    let mut line_buf = String::new();
    loop {
        line_buf.clear();
        match reader.read_line(&mut line_buf) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                log::warn!("read error: {e}");
                break;
            }
        }

        let line_bytes = line_buf.as_bytes();

        // Intercept subscribe_events before dispatch — we need the writer.
        if let Ok(text) = std::str::from_utf8(line_bytes) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim_end()) {
                if v.is_object() {
                    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
                    let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    if Method::from_str(method) == Some(Method::SubscribeEvents) {
                        // Register subscriber and forward events on a dedicated thread.
                        let rx = subscribers::register(&ctx.state);
                        if let Ok(ev_stream) = writer.try_clone() {
                            subscribers::forward_events(ev_stream, rx);
                        }
                        let ack = encode_response(id, serde_json::json!({ "subscribed": true }));
                        if writer.write_all(ack.as_bytes()).is_err() {
                            break;
                        }
                        continue;
                    }
                }
            }
        }

        let response = dispatch_with_ctx(&ctx, line_bytes);
        if writer.write_all(response.as_bytes()).is_err() {
            break;
        }
    }

    log::debug!("client disconnected");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_default_contains_auto2fa() {
        std::env::remove_var("AUTO2FA_SOCK");
        let p = socket_path();
        assert!(
            p.to_string_lossy().contains("auto2fa"),
            "path should contain auto2fa: {p:?}"
        );
    }

    #[test]
    fn socket_path_override_via_env() {
        std::env::set_var("AUTO2FA_SOCK", "/tmp/test_override.sock");
        let p = socket_path();
        assert_eq!(p, PathBuf::from("/tmp/test_override.sock"));
        std::env::remove_var("AUTO2FA_SOCK");
    }

    #[test]
    fn lock_path_override_via_env() {
        std::env::set_var("AUTO2FA_LOCK", "/tmp/test_override.lock");
        let p = lock_path();
        assert_eq!(p, PathBuf::from("/tmp/test_override.lock"));
        std::env::remove_var("AUTO2FA_LOCK");
    }
}
