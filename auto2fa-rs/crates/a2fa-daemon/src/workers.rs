//! Per-host worker infrastructure and OTP-group-lock registry.
//!
//! # OTP serialization (mirrors backend.py `_get_otp_group_lock` + `_fresh_otp_or_wait`)
//!
//! Many sites (e.g. Harvard FAS-RC) configure every login host with the same
//! Duo TOTP secret.  When the daemon brings several such hosts up in parallel,
//! naive code would derive the same 6-digit code from each, send them
//! simultaneously, and the server would consume the first while rejecting the
//! rest as replays ("looped back to Password prompt" cascade).
//!
//! Guard plan (mirrors Python exactly):
//!  1. Group hosts by a stable hash of the secret — only hosts sharing a
//!     secret block each other; hosts with distinct secrets run in parallel.
//!  2. Serialize the OTP *submission* per group with a `Mutex<OtpGroupState>`.
//!  3. After submitting, remember the code + timestamp.  The next caller
//!     regenerates; if the regenerated code matches the last-submitted code
//!     AND the window hasn't rolled over, release the lock, sleep until the
//!     next 30-second boundary, then re-acquire and re-check.
//!
//! # Per-host worker
//!
//! `spawn_host_start` runs `start_master` (blocking ssh pty) on a dedicated
//! OS thread, never holding `Mutex<State>` across the I/O.
//! On completion it re-locks State and writes the result back.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use log::{info, warn};

use a2fa_core::engine::State;
use a2fa_core::ssh::master::{start_master, stop_all, PoolState};
use a2fa_core::totp::totp_now;

// ---------------------------------------------------------------------------
// OTP-group-lock registry
// ---------------------------------------------------------------------------

/// Registry of per-secret-group mutex objects.
///
/// Key: stable 16-hex-char hash of the TOTP secret.
/// Value: `Arc<Mutex<OtpGroupState>>` shared by all hosts with that secret.
#[derive(Default)]
pub struct OtpRegistry {
    groups: Mutex<HashMap<String, Arc<Mutex<OtpGroupState>>>>,
}

/// State shared within one OTP secret group.
pub struct OtpGroupState {
    /// The code most recently submitted, plus the Unix timestamp (as f64) of
    /// the submission.
    pub last_submitted: Option<(String, f64)>,
}

// (The old fixed `TOTP_WINDOW_SEC = 30.0` constant is gone: the replay guard
// now derives the window from the secret's own period via totp::token_period.)

/// Deterministic, non-cryptographic hash of `secret` → 16-char hex key.
///
/// Uses FNV-1a so it is identical across process restarts (unlike
/// `std::collections::hash_map::DefaultHasher` which randomises its seed).
/// Parity with Python: we only need grouping identity (same secret → same
/// key), not security.
fn otp_group_key(secret: &str) -> String {
    let h: u64 = secret
        .bytes()
        .fold(14_695_981_039_346_656_037u64, |acc, b| {
            acc.wrapping_mul(1_099_511_628_211).wrapping_add(b as u64)
        });
    format!("{h:016x}")
}

impl OtpRegistry {
    /// Create a new, empty registry.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Return the group state for `secret`, creating it if needed.
    pub fn get_group(&self, secret: &str) -> Arc<Mutex<OtpGroupState>> {
        let key = otp_group_key(secret);
        let mut map = self.groups.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(key)
            .or_insert_with(|| {
                Arc::new(Mutex::new(OtpGroupState {
                    last_submitted: None,
                }))
            })
            .clone()
    }
}

// ---------------------------------------------------------------------------
// OTP closure builder (mirrors `_fresh_otp_or_wait` in backend.py)
// ---------------------------------------------------------------------------

/// Current Unix time as f64 seconds.
fn now_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Build an OTP closure suitable for passing to `start_master`.
///
/// The returned closure:
///  1. Acquires the per-group lock (serializing OTP submissions for hosts
///     that share this TOTP secret).
///  2. Generates a fresh TOTP code.
///  3. If the code matches the last-submitted code and the TOTP window has
///     NOT rolled over, releases the lock, sleeps until the next 30-second
///     window boundary (+1 s buffer), then re-acquires and re-checks.
///  4. Records the submission timestamp and returns the code.
///
/// The lock is released BEFORE sleeping (mirrors Python's lock-release-sleep
/// pattern so peers with the same secret aren't stalled for 30 s).
pub fn make_otp_closure(
    secret: String,
    host: String,
    registry: Arc<OtpRegistry>,
) -> impl Fn() -> a2fa_core::error::Result<String> {
    // The secret's ACTUAL step — hardcoding 30 let a 60s-period secret pass
    // the staleness check >35s into its window and re-submit the SAME code
    // (server-side replay rejection). Parsed once; bad secrets fall back to
    // 30 here and fail loudly inside totp_now below.
    let period = a2fa_core::totp::token_period(&secret).unwrap_or(30).max(1);
    move || {
        let group_arc = registry.get_group(&secret);
        loop {
            // Acquire the group lock — serializes OTP submission.
            let mut grp = group_arc.lock().unwrap_or_else(|e| e.into_inner());

            let code = totp_now(&secret)?;

            // Check whether this code was recently submitted within the
            // current TOTP window.
            let should_wait = match &grp.last_submitted {
                Some((last_code, last_ts)) => {
                    let age = now_f64() - last_ts;
                    last_code == &code && age < (period as f64 + 5.0)
                }
                None => false,
            };

            if !should_wait {
                // Fresh code — record and return while still holding the lock.
                grp.last_submitted = Some((code.clone(), now_f64()));
                info!("[{host}] OTP submitted");
                return Ok(code);
            }

            // Same code as last submission — release the lock before sleeping
            // so other hosts with this secret can proceed.
            let unix_now = now_f64() as u64;
            let secs_into_window = unix_now % period;
            let wait_secs = period - secs_into_window + 1;
            info!(
                "[{host}] OTP would replay last submission; \
                 waiting {wait_secs}s for next TOTP window"
            );
            drop(grp); // release lock BEFORE sleeping
            std::thread::sleep(Duration::from_secs(wait_secs));
            // Loop: re-acquire + re-check with the (likely new) code.
        }
    }
}

// ---------------------------------------------------------------------------
// Per-host worker: spawn master-start off the State mutex
// ---------------------------------------------------------------------------

/// Spawn a blocking OS thread that runs `start_master` for `host` at pool
/// `slot`, then writes the result back to `State`.
///
/// # Lock rule (never hold `Mutex<State>` across ssh I/O)
/// 1. Caller ensures credentials are already extracted from State before call.
/// 2. Thread does `start_master` (blocking pty ssh) with no lock held.
/// 3. Thread locks State → writes result → unlocks.
pub fn spawn_host_start(
    host_name: String,
    slot: usize,
    password: String,
    secret: String,
    registry: Arc<OtpRegistry>,
    state: Arc<Mutex<State>>,
) {
    let spawn_host = host_name.clone();
    let spawn_state = Arc::clone(&state);
    let spawn_res = std::thread::Builder::new()
        .name(format!("host-start:{host_name}"))
        .spawn(move || {
            // Build a PoolState for the single `start_master` call.  The
            // engine does not yet store PoolState per-host; for the wired
            // handlers we rebuild it fresh (fast, no I/O).  The active
            // symlink persists on disk so subsequent control checks still work.
            let mut pool = PoolState::new(&host_name);

            let otp_closure = make_otp_closure(secret, host_name.clone(), registry);

            info!("[{host_name}] host-start worker: spawning master slot {slot}");
            let ready = start_master(&mut pool, slot, &password, otp_closure);

            // Write result back to State (fast, no I/O).
            let mut guard = crate::lock_state(&state);
            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                if ready {
                    h.is_master_ready = true;
                    h.pool_alive = 1;
                    h.pool_index = slot as u8;
                    h.status = "Connected".into();
                    h.last_msg = format!("Master slot {slot} ready");
                    info!("[{host_name}] master ready — State updated");
                } else {
                    h.is_master_ready = false;
                    h.status = "Failed".into();
                    h.last_msg = format!("Master slot {slot} login failed");
                    warn!("[{host_name}] master failed — State updated");
                }
            }
        });
    if let Err(e) = spawn_res {
        // Transient EAGAIN: the worker never ran. Reset the host status to
        // Failed so the UI / maintenance reflects reality instead of a stuck
        // "Connecting".
        warn!("[{spawn_host}] failed to spawn host-start thread: {e}");
        let mut guard = crate::lock_state(&spawn_state);
        if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == spawn_host) {
            h.is_master_ready = false;
            h.status = "Failed".into();
            h.last_msg = format!("spawn failed: {e}");
        }
    }
}

/// Spawn a blocking OS thread that runs `stop_all` for `host`, then clears
/// the host's status in State.
pub fn spawn_host_stop(host_name: String, state: Arc<Mutex<State>>) {
    let spawn_host = host_name.clone();
    let spawn_res = std::thread::Builder::new()
        .name(format!("host-stop:{host_name}"))
        .spawn(move || {
            let mut pool = PoolState::new(&host_name);
            info!("[{host_name}] host-stop worker: stopping all master slots");
            stop_all(&mut pool);

            let mut guard = crate::lock_state(&state);
            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                h.is_master_ready = false;
                h.pool_alive = 0;
                h.active = false;
                h.status = "Idle".into();
                h.last_msg = "Stopped".into();
            }
        });
    if let Err(e) = spawn_res {
        // Transient EAGAIN: the stop worker never ran. Nothing is wedged (no
        // in-flight token), so just log — the master, if any, is cleaned up on
        // the next teardown / restart cycle.
        warn!("[{spawn_host}] failed to spawn host-stop thread: {e}");
    }
}

// ---------------------------------------------------------------------------
// Tunnel worker: spawn a forward start off the State mutex
// ---------------------------------------------------------------------------

/// Result written back to State after a tunnel-start attempt.
pub struct TunnelStartResult {
    pub name: String,
    pub ok: bool,
    pub msg: String,
    pub active_jump: Option<String>,
}

/// Spawn a blocking thread that runs `start_forward` + `probe_and_settle` for
/// `name`, then writes the result back to State and optionally runs
/// `post_connect`.
///
/// `jump`, `user`, `node`, `local_port`, `remote_port` are extracted from
/// State by the caller before this function is called (no lock held here).
pub fn spawn_tunnel_start(
    name: String,
    spec: a2fa_core::tunnels::forward::ForwardSpec,
    local_port: u16,
    remote_port: u16,
    post_connect_cmd: Option<String>,
    state: Arc<Mutex<State>>,
    post_connect_running: Arc<Mutex<std::collections::HashSet<String>>>,
) {
    spawn_tunnel_start_inner(
        name, spec, local_port, remote_port, post_connect_cmd, state,
        post_connect_running, None,
    );
}

/// Same as `spawn_tunnel_start` but also stores the `Child` in the
/// `TunnelRuntime` registry and sets `alive_since` on success.
///
/// Used by the IPC `tunnel_start` handler when a `TunnelRuntime` is available.
pub fn spawn_tunnel_start_with_runtime(
    name: String,
    spec: a2fa_core::tunnels::forward::ForwardSpec,
    local_port: u16,
    remote_port: u16,
    post_connect_cmd: Option<String>,
    state: Arc<Mutex<State>>,
    post_connect_running: Arc<Mutex<std::collections::HashSet<String>>>,
    runtime: Arc<crate::tunnel_runtime::TunnelRuntime>,
) {
    spawn_tunnel_start_inner(
        name, spec, local_port, remote_port, post_connect_cmd, state,
        post_connect_running, Some(runtime),
    );
}

/// Internal implementation shared by the two public variants above.
fn spawn_tunnel_start_inner(
    name: String,
    spec: a2fa_core::tunnels::forward::ForwardSpec,
    local_port: u16,
    remote_port: u16,
    post_connect_cmd: Option<String>,
    state: Arc<Mutex<State>>,
    post_connect_running: Arc<Mutex<std::collections::HashSet<String>>>,
    runtime: Option<Arc<crate::tunnel_runtime::TunnelRuntime>>,
) {
    let spawn_name = name.clone();
    let spawn_state = Arc::clone(&state);
    let spawn_res = std::thread::Builder::new()
        .name(format!("tunnel-start:{name}"))
        .spawn(move || {
            use a2fa_core::tunnels::forward::{probe_and_settle, start_forward_spec, ForwardSpec, ProbeOutcome};
            use a2fa_core::tunnels::post_connect::run_post_connect;
            use a2fa_core::model::TunnelStatus;

            let label = spec.label().to_string();
            // Post-connect needs a (node, jump) pair; for direct, host stands in for both.
            let (pc_node, pc_jump) = match &spec {
                ForwardSpec::Compute { node, jump, .. } => (node.clone(), jump.clone()),
                ForwardSpec::Direct { host } => (host.clone(), host.clone()),
            };
            // Human target for the connect record.
            let target = match &spec {
                ForwardSpec::Compute { node, .. } => format!("{node}:{remote_port}"),
                ForwardSpec::Direct { host } => format!("{host}:{remote_port} (direct)"),
            };

            info!("[tunnel:{name}] starting via {label}");

            let child = match start_forward_spec(&spec, local_port, remote_port) {
                Ok(c) => c,
                Err(e) => {
                    warn!("[tunnel:{name}] spawn failed: {e}");
                    let msg = format!("spawn failed: {e}");
                    if let Some(rt) = &runtime {
                        let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64();
                        rt.record(&name, ts, &msg);
                    }
                    let mut guard = crate::lock_state(&state);
                    if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == name) {
                        t.status = TunnelStatus::Failed;
                        t.last_msg = msg;
                        t.active_jump = None;
                    }
                    return;
                }
            };

            let timeout = std::time::Duration::from_secs(10);
            match probe_and_settle(child, local_port, timeout) {
                Ok((ProbeOutcome::Ready, child)) => {
                    // Tunnel is alive.
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64();

                    // The user may have hit Stop during the start (ssh spawn +
                    // ~10 s probe) — while `Starting`, the child isn't in the
                    // registry yet, so tunnel_stop had nothing to kill. Honor
                    // the abort HERE instead of resurrecting: the old
                    // unconditional `wants_alive = true` write overwrote (and
                    // persisted!) the user's stop.
                    let aborted = {
                        let guard = crate::lock_state(&state);
                        guard
                            .tunnels
                            .iter()
                            .find(|t| t.name == name)
                            .map(|t| !t.wants_alive)
                            .unwrap_or(true) // tunnel deleted mid-start → abort
                    };
                    if aborted {
                        info!("[tunnel:{name}] stopped during start — killing fresh forward (abort honored)");
                                                a2fa_core::tunnels::forward::stop_forward(child);
                        if let Some(rt) = &runtime {
                            rt.record(&name, now, "start aborted by user stop");
                        }
                        return;
                    }

                    // Store child in runtime registry if available.
                    if let Some(rt) = &runtime {
                        rt.store_child(&name, child);
                    }

                    // Commit Alive UNDER a wants_alive re-check, closing the tiny
                    // TOCTOU window between the abort check above and this write: a
                    // Stop landing in that gap (now AFTER the child is stored) must
                    // win, not be resurrected and persisted as wants_alive=true.
                    let committed = {
                        let mut guard = crate::lock_state(&state);
                        match guard.tunnels.iter_mut().find(|t| t.name == name) {
                            Some(t) if t.wants_alive => {
                                t.status = TunnelStatus::Alive;
                                t.last_alive_at = now;
                                t.connect_count += 1;
                                t.active_jump = Some(label.clone());
                                t.last_msg = format!("via {label}");
                                info!("[tunnel:{name}] alive via {label}");
                                true
                            }
                            _ => false, // user stopped (or deleted) during start
                        }
                    };
                    if !committed {
                        info!("[tunnel:{name}] stopped during start (post-store) — killing fresh forward (abort honored)");
                        if let Some(rt) = &runtime {
                            rt.kill_child(&name);
                            rt.record(&name, now, "start aborted by user stop");
                        }
                        return;
                    }

                    // Runtime bookkeeping only after the Alive commit succeeds.
                    if let Some(rt) = &runtime {
                        rt.with_rt_mut(&name, |r| r.alive_since = Some(now));
                        rt.record(&name, now, format!("connected via {label} → {target}"));
                    }

                    // Persist wants_alive (off-lock + through the serialized
                    // save helper — a raw snapshot-then-save here could rename
                    // a stale snapshot over a newer tunnel_add save).
                    crate::handlers::tunnels::persist_tunnels(&state);

                    // Post-connect hook.
                    if let Some(cmd) = post_connect_cmd {
                        run_post_connect(
                            name.clone(),
                            cmd,
                            local_port,
                            pc_node.clone(),
                            pc_jump.clone(),
                            post_connect_running,
                        );
                    }
                }
                Ok((outcome @ (ProbeOutcome::TimedOut | ProbeOutcome::ChildExited), mut child)) => {
                    let msg = match outcome {
                        ProbeOutcome::ChildExited => {
                            // Reap the exited child (TimedOut already kills+waits
                            // inside probe_and_settle; ChildExited returns it
                            // un-reaped).
                            let _ = child.kill();
                            let _ = child.wait();
                            format!("local port {local_port} in use by another process (ssh exited at bind)")
                        }
                        _ => "probe timed out".to_string(),
                    };
                    warn!("[tunnel:{name}] {msg}");
                    if let Some(rt) = &runtime {
                        let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64();
                        rt.record(&name, ts, &msg);
                    }
                    let mut guard = crate::lock_state(&state);
                    if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == name) {
                        t.fail_count += 1;
                        t.status = TunnelStatus::Failed;
                        t.last_msg = msg;
                        t.active_jump = None;
                    }
                }
                Err(e) => {
                    warn!("[tunnel:{name}] probe error: {e}");
                    let msg = format!("probe error: {e}");
                    if let Some(rt) = &runtime {
                        let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64();
                        rt.record(&name, ts, &msg);
                    }
                    let mut guard = crate::lock_state(&state);
                    if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == name) {
                        t.fail_count += 1;
                        t.status = TunnelStatus::Failed;
                        t.last_msg = msg;
                        t.active_jump = None;
                    }
                }
            }
        });
    if let Err(e) = spawn_res {
        // Transient EAGAIN: the worker never ran, so the tunnel is stuck at the
        // caller-set Starting status. Reset it to Failed (under the lock) so the
        // maintenance loop's Recover picks it up on a later tick.
        warn!("[tunnel:{spawn_name}] failed to spawn tunnel-start thread: {e}");
        let mut guard = crate::lock_state(&spawn_state);
        if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == spawn_name) {
            t.status = a2fa_core::model::TunnelStatus::Failed;
            t.last_msg = format!("spawn failed: {e}");
            t.active_jump = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ---- OtpRegistry -------------------------------------------------------

    #[test]
    fn same_secret_gives_same_group() {
        let reg = OtpRegistry::new();
        let g1 = reg.get_group("MYSECRET");
        let g2 = reg.get_group("MYSECRET");
        // Same Arc pointer ↔ same group lock.
        assert!(Arc::ptr_eq(&g1, &g2));
    }

    #[test]
    fn different_secrets_give_different_groups() {
        let reg = OtpRegistry::new();
        let g1 = reg.get_group("SECRET_A");
        let g2 = reg.get_group("SECRET_B");
        assert!(!Arc::ptr_eq(&g1, &g2));
    }

    #[test]
    fn otp_group_key_is_deterministic() {
        assert_eq!(otp_group_key("hello"), otp_group_key("hello"));
        assert_ne!(otp_group_key("hello"), otp_group_key("world"));
    }

    // ---- OTP serialization: same secret → one inside the group at a time --

    #[test]
    fn otp_group_lock_serializes_same_secret() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let registry = OtpRegistry::new();
        let secret = "JBSWY3DPEHPK3PXP"; // standard TOTP test vector
        let inside_count = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));

        let threads: Vec<_> = (0..4)
            .map(|_| {
                let reg = registry.clone();
                let ic = inside_count.clone();
                let mc = max_concurrent.clone();
                let s = secret.to_string();
                std::thread::spawn(move || {
                    let group = reg.get_group(&s);
                    let _guard = group.lock().unwrap_or_else(|e| e.into_inner());
                    // Inside the lock — measure concurrency.
                    let prev = ic.fetch_add(1, Ordering::SeqCst);
                    mc.fetch_max(prev + 1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(5));
                    ic.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();

        for t in threads {
            t.join().unwrap();
        }

        // At most 1 thread should have been inside the lock at once.
        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            1,
            "more than 1 thread was concurrently inside the OTP group lock"
        );
    }

    // ---- Different secrets → no blocking between groups --------------------

    #[test]
    fn otp_group_lock_does_not_serialize_different_secrets() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Barrier;

        let registry = OtpRegistry::new();
        let inside_count = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        // Barrier so both threads enter simultaneously.
        let barrier = Arc::new(Barrier::new(2));

        let secrets = ["SECRET_ALPHA_111", "SECRET_BETA_222"];
        let threads: Vec<_> = secrets
            .iter()
            .map(|&s| {
                let reg = registry.clone();
                let ic = inside_count.clone();
                let mc = max_concurrent.clone();
                let b = barrier.clone();
                let secret = s.to_string();
                std::thread::spawn(move || {
                    let group = reg.get_group(&secret);
                    // Both threads wait here so they enter the lock attempt together.
                    b.wait();
                    let _guard = group.lock().unwrap_or_else(|e| e.into_inner());
                    let prev = ic.fetch_add(1, Ordering::SeqCst);
                    mc.fetch_max(prev + 1, Ordering::SeqCst);
                    // Hold for a moment to make overlap visible.
                    std::thread::sleep(Duration::from_millis(20));
                    ic.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();

        for t in threads {
            t.join().unwrap();
        }

        // Different secrets → different locks → both run in parallel.
        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            2,
            "threads with different secrets should be able to run concurrently"
        );
    }
}
