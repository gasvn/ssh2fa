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

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Hard cap on concurrently-handled connections. Each connection holds a
/// thread for its lifetime, so a buggy/looping local client must not be able
/// to spawn threads without bound.
const MAX_CONNS: usize = 128;

/// RAII guard: increments the live-connection counter on construction and
/// decrements it on drop. Guarantees the counter is released on EVERY exit
/// path from a connection handler — normal return, early return, or panic.
struct ConnGuard(Arc<AtomicUsize>);

impl ConnGuard {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        ConnGuard(counter)
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

use anyhow::{Context, Result};
use a2fa_core::config::{config_dir, passwords_path};
use a2fa_core::engine::{tick::poll_loop, State};
use a2fa_core::proto::{encode_error, encode_response, ErrCode, Method};

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
    //        Bounded: migrate_v1_to_v2 writes to the Keychain (SecItem*, which
    //        serializes process-wide and blocks indefinitely on a locked /
    //        prompting Keychain). Run it on a worker with a hard join timeout so
    //        a locked Keychain at login/reboot can never wedge the boot thread
    //        (which would never reach the accept loop → launchd respawns into
    //        the same wedge → hard-reboot-only crashloop). On the already-v2
    //        machine this short-circuits instantly. On timeout we proceed with
    //        the file still v1 (load yields 0 hosts; migration retries next
    //        launch) rather than block forever.
    //        The worker does only the (blocking) Keychain writes via
    //        prepare_migration and RETURNS the v2 host map; THIS (boot) thread
    //        does the final save_meta. That guarantees exactly one thread ever
    //        writes passwords.json: an abandoned/timed-out worker never persists,
    //        so it can't race a later host_add save_meta (a lost-update). save_meta
    //        here runs before State::new + the accept loop, so nothing else writes
    //        the file concurrently either.
    {
        let passwords_for_migrate = passwords_p.clone();
        type MigrateMsg = std::result::Result<
            Option<std::collections::HashMap<String, a2fa_core::config::HostMeta>>,
            String,
        >;
        let (tx, rx) = std::sync::mpsc::sync_channel::<MigrateMsg>(1);
        let spawn_res = std::thread::Builder::new()
            .name("creds-migrate".into())
            .spawn(move || {
                let kc = a2fa_core::creds::keychain::KeychainStore;
                let r = a2fa_core::creds::migrate::prepare_migration(&kc, &passwords_for_migrate)
                    .map_err(|e| e.to_string());
                let _ = tx.send(r);
            });
        match spawn_res {
            Ok(_) => match rx.recv_timeout(std::time::Duration::from_secs(15)) {
                // commit_migration_meta, NOT save_meta: this is the one writer
                // allowed to replace the legacy v1 file (save_meta refuses).
                Ok(Ok(Some(hosts))) => match a2fa_core::config::commit_migration_meta(&passwords_p, &hosts) {
                    Ok(_) => log::info!("migrated passwords.json v1 -> v2 (creds moved to Keychain)"),
                    Err(e) => log::error!("migration: persisting v2 passwords.json failed: {e}"),
                },
                Ok(Ok(None)) => {}
                Ok(Err(e))   => log::error!("passwords.json migration failed (continuing): {e}"),
                Err(_)       => log::warn!(
                    "passwords.json migration timed out (Keychain locked?) — continuing un-migrated \
                     (file left v1, not persisted, no race). It will retry next launch. CAUTION: if a \
                     host is ADDED before migration succeeds, the new v2 file may omit the not-yet-\
                     migrated hosts' auto_connect metadata; recover from the <passwords.json>.\
                     pre-keychain-backup written before migration. Credentials themselves are safe in \
                     the Keychain."
                ),
            },
            Err(e) => log::error!("could not spawn migration worker ({e}); skipping migration this run"),
        }
    }

    // Pre-load guards (single-threaded — the accept loop isn't running yet):
    // preserve an unparseable tunnels.json before the first persist would
    // silently destroy it, and sweep temp files leaked by SIGKILL deploys.
    a2fa_core::config::backup_if_unparseable(&tunnels_path);
    a2fa_core::config::sweep_stale_tmp(&tunnels_path);

    let state = Arc::new(Mutex::new(State::new(tunnels_path, &passwords_p)));

    {
        let guard = crate::lock_state(&state);
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
        // Degrade, never crash: a spawn Err must not propagate out of run() and
        // exit the process (main.rs does process::exit(1) on Err → launchd
        // respawn → boot_autostart spawn storm). Log and continue without the
        // tick loop; the daemon still serves IPC.
        if let Err(e) = std::thread::Builder::new()
            .name("tick-loop".into())
            .spawn(move || poll_loop(&state2, &stop2))
        {
            log::error!("failed to spawn tick thread ({e}); periodic tick disabled this run");
        }
    }

    // 6b. Spawn heartbeat / auto-reconnect thread.
    //     Loops every ~3 s, heartbeats each active host's pool slots, and
    //     restarts dead masters — the Rust port of `manage_pool_loop`.
    start_heartbeat(Arc::clone(&state), Arc::clone(&managers), Arc::clone(&registry));

    // 6c. Spawn tunnel maintenance thread.
    //     Runs every ~1 s: auto-recovery, child-died detection, squeue/stale,
    //     and boot auto-start — the Rust port of `TunnelManager.tick()`.
    use std::collections::HashSet;
    // The ONE post-connect dedup set, shared between the maintenance loop AND
    // the IPC `tunnel_start`/`tunnels_batch` paths (via DaemonCtx). A fresh set
    // per IPC call would make dedup a no-op across paths.
    let post_connect_running: Arc<std::sync::Mutex<HashSet<String>>> =
        Arc::new(std::sync::Mutex::new(HashSet::new()));
    start_tunnel_maintenance(
        Arc::clone(&state),
        Arc::clone(&runtime),
        Arc::clone(&post_connect_running),
    );

    log::info!("daemon listening on {}", sock_p.display());
    println!("a2fa-daemon listening on {}", sock_p.display());

    // Build the shared daemon context (cloned cheaply per connection).
    let ctx = DaemonCtx {
        state: Arc::clone(&state),
        managers: Arc::clone(&managers),
        registry: Arc::clone(&registry),
        runtime: Arc::clone(&runtime),
        // One shared guard for the whole daemon: the two Mac wake monitors fire
        // wake_recover together, so closely-following calls must coalesce.
        wake_recover_guard: crate::handlers::system::WakeRecoverGuard::new(),
        // Share the SAME post-connect dedup set the maintenance loop uses.
        post_connect_running: Arc::clone(&post_connect_running),
    };

    // 6d. Spawn signal-handler thread.
    //     Blocks on the next SIGINT or SIGTERM, then tears down SSH masters,
    //     kills tunnel children, removes the socket, and exits.
    {
        use signal_hook::consts::{SIGINT, SIGTERM};
        use signal_hook::iterator::Signals;

        let mut signals = Signals::new([SIGINT, SIGTERM]).context("install signal handler")?;
        let stop_sig = Arc::clone(&stop);
        let managers_sig = Arc::clone(&managers);
        let runtime_sig = Arc::clone(&runtime);
        let sock_sig = sock_p.clone();
        std::thread::Builder::new()
            .name("signal".into())
            .spawn(move || {
                if let Some(sig) = signals.forever().next() {
                    log::info!("received signal {sig}; shutting down gracefully");
                    stop_sig.store(true, std::sync::atomic::Ordering::Relaxed);
                    managers_sig.teardown_all();
                    runtime_sig.kill_all_children();
                    let _ = std::fs::remove_file(&sock_sig);
                    log::info!("graceful shutdown complete");
                    std::process::exit(0);
                }
            })
            .context("spawn signal thread")?;
    }

    // 7. Accept loop.
    let conn_count = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                // Connection cap: refuse (close) when too many are live, rather
                // than spawning an unbounded number of handler threads.
                if conn_count.load(Ordering::SeqCst) >= MAX_CONNS {
                    log::warn!(
                        "refusing connection: MAX_CONNS ({MAX_CONNS}) reached; closing"
                    );
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    drop(stream);
                    continue;
                }
                let ctx2 = ctx.clone();
                // Claim the connection slot BEFORE spawning: incrementing inside
                // the spawned thread let a connect burst race past the cap check
                // above (several spawns before any increment landed). If the
                // spawn fails the closure — and the guard moved into it — is
                // dropped, releasing the slot. RAII covers every path.
                let guard = ConnGuard::new(Arc::clone(&conn_count));
                // A transient spawn failure (e.g. EAGAIN under fd/thread
                // pressure) must NOT kill the daemon: drop this connection,
                // log a warning, and keep accepting. Never `?`/propagate.
                if let Err(e) = std::thread::Builder::new()
                    .name("conn".into())
                    .spawn(move || {
                        let _guard = guard;
                        handle_connection(stream, ctx2);
                    })
                {
                    log::warn!("failed to spawn connection thread (dropping connection): {e}");
                    continue;
                }
            }
            Err(e) => {
                // A transient accept error (EMFILE/ENFILE/EAGAIN) must NOT
                // permanently stop the accept loop. Log, briefly back off to
                // avoid a tight error-spin on a persistent condition, then
                // keep accepting. The loop only ends when the listener is
                // intentionally closed.
                log::error!("accept error (continuing): {e}");
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
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

    // Socket timeouts: a slow-loris (never finishes a line) or a non-reading
    // client (its recv buffer fills so our write_all blocks) must not pin a
    // bounded handler thread forever. A read timeout makes read_line return
    // WouldBlock/TimedOut (handled as non-fatal below so idle subscribers
    // survive); a write timeout eventually fails a stuck write and frees the
    // thread.
    if let Err(e) = reader_stream.set_read_timeout(Some(Duration::from_secs(30))) {
        log::warn!("set_read_timeout failed: {e}");
    }
    if let Err(e) = stream.set_write_timeout(Some(Duration::from_secs(10))) {
        log::warn!("set_write_timeout failed: {e}");
    }

    let mut reader = BufReader::new(reader_stream);
    // Writes to this connection come from TWO threads once it subscribes: this
    // loop (request responses + the subscribe ack) and the subscriber forwarder
    // thread (events). Share the write half behind a lock so their
    // newline-delimited JSON frames can NEVER interleave on the fd — interleaved
    // bytes would corrupt the wire protocol and break the client's line parser.
    let writer = std::sync::Arc::new(std::sync::Mutex::new(stream));

    // Per-connection subscribe-once flag. A connection may send
    // `subscribe_events` repeatedly (the read loop `continue`s after handling
    // it); we register exactly ONE subscriber + forwarder thread per
    // connection and just re-ack any further requests. Without this, a single
    // connection could register unbounded subscribers/threads.
    let mut subscribed = false;

    // Hard cap on a single request line. A client streaming bytes with no
    // newline must not be able to grow our accumulation buffer without bound.
    const MAX_LINE_BYTES: usize = 1024 * 1024; // 1 MiB

    // Persistent byte accumulation buffer for the current (possibly partial)
    // line. It is cleared ONLY after a complete line has been dispatched — NOT
    // on the read-timeout `continue` path — so a line that straddles a 30s read
    // timeout keeps its already-read bytes instead of being silently truncated.
    let mut raw: Vec<u8> = Vec::new();
    loop {
        // Read up to the remaining budget for THIS line, stopping at a newline.
        // We bound the READ itself (not just a post-hoc len() check): a client
        // flooding bytes with no newline can read at most MAX_LINE_BYTES + 1
        // total across however many timeout-retries, so it can never OOM us.
        // `take()` limits THIS read to (remaining budget + 1); the budget is
        // `raw.len()` already accumulated from prior partial reads, so it is
        // correctly carried across WouldBlock/TimedOut retries.
        let budget = (MAX_LINE_BYTES + 1).saturating_sub(raw.len());
        match (&mut reader)
            .take(budget as u64)
            .read_until(b'\n', &mut raw)
        {
            Ok(0) => break, // EOF
            Ok(_) => {}
            // A read timeout fires on idle connections — notably subscriber
            // connections that block on read while events are pushed by the
            // separate forwarder thread. Treat it as NON-fatal: keep the
            // connection alive and retry WITHOUT clearing `raw`, so any partial
            // bytes already read for a straddling line are preserved (no
            // truncation). The wedged-write protection comes from the write
            // timeout.
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                continue;
            }
            Err(e) => {
                log::warn!("read error: {e}");
                break;
            }
        }

        // If we have not yet seen a terminating newline, the line is still
        // incomplete: either we hit our per-line byte budget (over-limit DoS)
        // or the read returned a short partial chunk. Distinguish the two: if
        // we already buffered MAX_LINE_BYTES + 1 bytes without a newline, the
        // line is over the cap → close. Otherwise keep accumulating.
        if raw.last() != Some(&b'\n') {
            if raw.len() > MAX_LINE_BYTES {
                log::warn!(
                    "request line exceeded {MAX_LINE_BYTES} bytes without a newline; closing connection"
                );
                break;
            }
            // Short read without a newline yet — accumulate and read more.
            continue;
        }

        // A complete newline-delimited line is in `raw`. Decode to bytes for
        // dispatch, then clear the accumulation buffer for the NEXT line (only
        // here, after a full line — never on the timeout-continue path).
        let line_owned = std::mem::take(&mut raw);
        let line_bytes = line_owned.as_slice();

        // Intercept subscribe_events before dispatch — we need the writer.
        if let Ok(text) = std::str::from_utf8(line_bytes) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim_end()) {
                if v.is_object() {
                    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
                    let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    if Method::from_str(method) == Some(Method::SubscribeEvents) {
                        // Register exactly once per connection.
                        if !subscribed {
                            match subscribers::register(&ctx.state) {
                                Some(rx) => {
                                    // Share the locked write half with the
                                    // forwarder so events + responses serialize.
                                    subscribers::forward_events(
                                        std::sync::Arc::clone(&writer),
                                        rx,
                                    );
                                    subscribed = true;
                                }
                                None => {
                                    // Cap reached — refused by the engine.
                                    log::warn!(
                                        "subscribe_events refused (subscriber cap reached)"
                                    );
                                }
                            }
                        }
                        let ack = encode_response(id, serde_json::json!({ "subscribed": subscribed }));
                        if writer
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .write_all(ack.as_bytes())
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                }
            }
        }

        // Guard the handler dispatch: a panic inside a handler must not unwind
        // (and kill) this connection thread. If a handler panicked while
        // holding the State lock the lock is already poisoned — the loops are
        // poison-tolerant for that — but catching here keeps the IPC surface
        // alive and returns a clean internal error to the client.
        let response = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dispatch_with_ctx(&ctx, line_bytes)
        })) {
            Ok(resp) => resp,
            Err(_) => {
                log::error!("handler panicked during dispatch; returning internal_error");
                encode_error("", ErrCode::Internal, "internal error")
            }
        };
        if writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write_all(response.as_bytes())
            .is_err()
        {
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
