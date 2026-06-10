//! IPC handlers for system-level methods.
//!
//! Methods: log_tail, wake_recover, reset_all, subscribe_events.
//!
//! Parity: `Auto2FADaemon.handle_request` in daemon.py.

use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use a2fa_core::engine::State;
use a2fa_core::error::{Error, Result};
use a2fa_core::model::TunnelStatus;
use serde_json::{json, Value};

use crate::managers::{self, active_host_names};

// ---------------------------------------------------------------------------
// wake_recover coalescing guard
// ---------------------------------------------------------------------------

/// Minimum interval (seconds) between two real `wake_recover` runs. A second
/// wake that arrives within this window after the previous run *completed* is
/// coalesced into a no-op. Two independent Mac monitors (SleepWakeMonitor +
/// NetworkMonitor) fire on a single wake, so without this the inline 5s×N
/// `ssh -O check` probe loop would run several times over.
const WAKE_RECOVER_MIN_INTERVAL_SECS: u64 = 12;

/// Daemon-global coalescing guard for `wake_recover`.
///
/// One shared instance lives in [`DaemonCtx`] (created once in `server.rs`).
/// It enforces two things so overlapping/closely-following `wake_recover`
/// calls collapse to a single real run:
///
/// 1. **In-flight flag** (`in_flight`): only one `wake_recover` runs the inline
///    probe loop at a time; concurrent callers bail out immediately.
/// 2. **Min-interval debounce** (`last_completed`): a wake arriving < ~12s after
///    the previous run *finished* is treated as redundant and skipped.
///
/// Claim is made via [`WakeRecoverGuard::try_claim`], which returns an RAII
/// [`WakeRecoverClaim`] on success. The claim records the completion timestamp
/// and clears the in-flight flag in its `Drop`, so the flag is released on
/// *every* exit path (early return, `?`, or panic-unwind).
#[derive(Debug)]
pub struct WakeRecoverGuard {
    in_flight: AtomicBool,
    /// Unix seconds at which the last real run completed (0 = never).
    last_completed: AtomicU64,
}

impl WakeRecoverGuard {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            in_flight: AtomicBool::new(false),
            last_completed: AtomicU64::new(0),
        })
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Try to claim the right to run `wake_recover`.
    ///
    /// Returns `Some(claim)` for the one caller that should run, or `None` if a
    /// run is already in flight or the debounce window has not elapsed (the
    /// caller should return an "already recovering" no-op).
    pub fn try_claim(self: &Arc<Self>) -> Option<WakeRecoverClaim> {
        // In-flight check first: a concurrent caller bails immediately.
        if self.in_flight.swap(true, Ordering::SeqCst) {
            return None;
        }
        // We now hold the in-flight flag. Check the debounce window; if it has
        // not elapsed, release the flag and bail (so we don't leak it).
        let now = Self::now_secs();
        let last = self.last_completed.load(Ordering::SeqCst);
        if last != 0 && now.saturating_sub(last) < WAKE_RECOVER_MIN_INTERVAL_SECS {
            self.in_flight.store(false, Ordering::SeqCst);
            return None;
        }
        Some(WakeRecoverClaim {
            guard: Arc::clone(self),
        })
    }
}

/// RAII token proving the holder won the right to run `wake_recover`.
///
/// On `Drop` it stamps the completion time and clears the in-flight flag, so
/// the guard is released on every exit path (return, `?`, panic).
pub struct WakeRecoverClaim {
    guard: Arc<WakeRecoverGuard>,
}

impl Drop for WakeRecoverClaim {
    fn drop(&mut self) {
        self.guard
            .last_completed
            .store(WakeRecoverGuard::now_secs(), Ordering::SeqCst);
        self.guard.in_flight.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// log_tail
// ---------------------------------------------------------------------------

/// Return the last `n` lines of `/tmp/auto2fa_daemon.log`.
///
/// Uses a backwards block-read to stay cheap on large files
/// (mirrors `_tail_file` in daemon.py).
pub fn log_tail(_state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    // Clamp: an unbounded `lines` would walk the WHOLE file backwards holding
    // every line in RAM (multi-GB log → handler thread pinned + OOM risk) over
    // a single bad request. 10k lines is far more than any UI shows.
    const MAX_TAIL_LINES: usize = 10_000;
    let n = params
        .get("lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(200)
        .min(MAX_TAIL_LINES as u64) as usize;

    let path = "/tmp/auto2fa_daemon.log";
    let lines = tail_file(path, n)?;
    Ok(json!({ "lines": lines }))
}

fn tail_file(path: &str, n: usize) -> Result<Vec<String>> {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(Error::Io(e)),
    };

    let size = f.seek(SeekFrom::End(0)).map_err(Error::Io)?;
    if size == 0 {
        return Ok(vec![]);
    }

    let block_size: u64 = 4096;
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut carry: Vec<u8> = Vec::new();
    let mut offset: u64 = size;

    while offset > 0 && lines.len() <= n {
        let read_size = block_size.min(offset);
        offset -= read_size;
        f.seek(SeekFrom::Start(offset)).map_err(Error::Io)?;
        let mut buf = vec![0u8; read_size as usize];
        f.read_exact(&mut buf).map_err(Error::Io)?;
        buf.extend_from_slice(&carry);
        let mut parts: Vec<Vec<u8>> = buf.split(|&b| b == b'\n').map(|s| s.to_vec()).collect();
        carry = parts.remove(0);
        parts.extend(lines);
        lines = parts;
    }
    if offset == 0 && !carry.is_empty() {
        let mut new = vec![carry];
        new.extend(lines);
        lines = new;
    }

    let decoded: Vec<String> = lines
        .into_iter()
        .filter(|l| !l.is_empty())
        .map(|l| String::from_utf8_lossy(&l).into_owned())
        .collect();

    let total = decoded.len();
    Ok(if total > n {
        decoded[total - n..].to_vec()
    } else {
        decoded
    })
}

// ---------------------------------------------------------------------------
// reset_all
// ---------------------------------------------------------------------------

/// User-triggered nuclear restart: stop every active tunnel and force-rebuild
/// every enabled master.  Mirrors `Auto2FADaemon._reset_all` in daemon.py.
///
/// # Lock discipline
/// The State lock is held only for brief bookkeeping (collecting tunnel names,
/// flipping status fields).  All blocking ssh work is done off-lock: tunnel
/// children are SIGKILLed via `ctx.runtime.kill_child` (fast, no State lock
/// held), and master rebuilds are dispatched to background threads by
/// `managers::rebuild_masters` (which never holds either lock across ssh I/O).
/// This handler returns immediately, like the Python version that schedules
/// the work.
pub fn reset_all(ctx: &crate::dispatch::DaemonCtx, _params: &Value) -> Result<Value> {
    // 1. Collect names of currently-active tunnels (brief lock) and flip them
    //    to Idle / not-wanted in the same critical section.
    let stopped: Vec<String> = {
        let mut guard = crate::lock_state(&ctx.state);
        let names: Vec<String> = guard
            .tunnels
            .iter()
            .filter(|t| {
                matches!(
                    t.status,
                    TunnelStatus::Alive | TunnelStatus::Starting | TunnelStatus::Stale
                )
            })
            .map(|t| t.name.clone())
            .collect();
        for name in &names {
            if let Some(t) = guard.tunnels.iter_mut().find(|t| &t.name == name) {
                t.status = TunnelStatus::Idle;
                t.wants_alive = false;
                t.active_jump = None;
                t.last_msg = "Stopped (reset_all)".into();
            }
        }
        names
    };

    // 2. Off-lock: kill each tunnel's ssh -L child (idempotent no-op if none).
    for name in &stopped {
        ctx.runtime.kill_child(name);
    }

    // 3. Force-rebuild every active host's master (background threads).
    //    reset_breakers=true: this is the user's EXPLICIT "reset everything"
    //    — clearing cooldown/flap backoffs is the point.
    let masters_rebuilt = managers::rebuild_masters(
        &active_host_names(&ctx.state),
        &ctx.state,
        &ctx.managers,
        &ctx.registry,
        true,
    );

    log::info!(
        "reset_all: stopped {} tunnels, rebuilding {} masters",
        stopped.len(),
        masters_rebuilt
    );

    Ok(json!({
        "tunnels_stopped": stopped.len(),
        "masters_rebuilt": masters_rebuilt,
    }))
}

// ---------------------------------------------------------------------------
// wake_recover
// ---------------------------------------------------------------------------

/// Restore connectivity after a Mac wake / network change.  Mirrors
/// `Auto2FADaemon._wake_recover` in daemon.py.
///
/// Probes each active host's master; rebuilds the ones that failed; then
/// restarts any tunnel whose jump master failed OR whose ssh -L child is dead.
///
/// # Restart strategy — divergence from Python
/// Python schedules a bespoke asyncio back-off retry (`WAKE_RETRY_DELAYS` =
/// 10/20/30/60/120s) to keep retrying `start()` until the tunnel goes alive.
/// This port deliberately does NOT build a separate retry thread.  Instead it
/// leaves `wants_alive = true` on each tunnel that needs restarting and lets
/// the daemon's always-on tunnel-maintenance auto-recovery loop bring it back:
/// that loop restarts any `wants_alive` tunnel whenever a ready jump master
/// exists (throttled to ~15s) and never gives up.  Setting `wants_alive = true`
/// is therefore sufficient and strictly more robust than Python's one-shot
/// schedule — and avoids duplicating the maintenance loop (no over-engineering).
///
/// # Lock discipline
/// The State lock is taken only for brief snapshots and field updates. The 5s
/// master probes (`ssh -O check`) run fully OFF both locks: we snapshot each
/// host's `active_index` under a brief map lock (`snapshot`, no I/O), drop the
/// lock, then call `master_check` on the concrete control path. Holding the
/// managers map lock across the blocking probe would stall the heartbeat loop
/// for up to 5s per host — the same "never hold a lock across ssh I/O" rule the
/// rest of the daemon follows. Master rebuilds + child kills also run off-lock.
pub fn wake_recover(ctx: &crate::dispatch::DaemonCtx, _params: &Value) -> Result<Value> {
    // 0. Coalesce overlapping / closely-following calls. Two independent Mac
    //    monitors fire wake_recover on a single wake; without this each one runs
    //    the inline 5s×N `ssh -O check` probe loop on its own connection thread.
    //    The first caller wins the claim and runs; everyone else returns an
    //    immediate no-op. The claim's Drop clears the in-flight flag + stamps the
    //    completion time on *every* exit path below (return / `?` / panic).
    let _claim = match ctx.wake_recover_guard.try_claim() {
        Some(c) => c,
        None => {
            log::info!("wake_recover: already recovering (coalesced), skipping");
            return Ok(json!({ "tunnels_restarting": [], "coalesced": true }));
        }
    };

    // 1. Snapshot the tunnels that were alive at wake time (name, active_jump).
    let alive_tunnels: Vec<(String, Option<String>)> = {
        let guard = crate::lock_state(&ctx.state);
        guard
            .tunnels
            .iter()
            .filter(|t| {
                matches!(
                    t.status,
                    TunnelStatus::Alive | TunnelStatus::Starting | TunnelStatus::Stale
                )
            })
            .map(|t| (t.name.clone(), t.active_jump.clone()))
            .collect()
    };

    // 2. Probe each active host's master. The blocking `ssh -O check` (5s) MUST
    //    run off both locks: snapshot the active slot index under a brief map
    //    lock, then check the concrete control path with no lock held. (Using
    //    `with_pool(.., active_master_ready)` would hold the map mutex across the
    //    5s probe and stall the heartbeat loop — see the lock-discipline note.)
    //    The probes run in PARALLEL (scoped threads): serially, a real
    //    network-down wake ran every probe to its full 5s deadline — 5s × N
    //    hosts on this connection-handler thread, starving the app's other
    //    requests. Parallel bounds the whole step at ~one probe deadline.
    let active_hosts = active_host_names(&ctx.state);
    let masters_failed: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = active_hosts
            .iter()
            .map(|host| {
                let idx = ctx.managers.snapshot(host).active_index; // brief lock, no I/O
                scope.spawn(move || {
                    let path = a2fa_core::ssh::control::control_path(host, idx);
                    let ready = a2fa_core::ssh::control::master_check(&path, host); // off-lock 5s
                    (host.clone(), ready)
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| match h.join() {
                Ok((host, ready)) if !ready => Some(host),
                // A panicked probe thread counts as "not failed" — rebuilding a
                // master on probe-infrastructure failure would burn 2FA for
                // nothing; the heartbeat's own health check backstops.
                _ => None,
            })
            .collect()
    });

    log::info!(
        "wake_recover: {} tunnels alive at wake, {} of {} masters failed",
        alive_tunnels.len(),
        masters_failed.len(),
        active_hosts.len()
    );

    // 3. Rebuild the masters that failed (background threads).
    //    reset_breakers=false — CRITICAL: wake_recover fires automatically on
    //    every network-up (two Mac monitors, ≥12s apart). Resetting the
    //    breakers here meant an oscillating network re-armed a fresh full
    //    login every up-phase forever (FAS-RC rate-limit incident class).
    //    Hosts in cooldown stay in cooldown; the heartbeat recovers them.
    managers::rebuild_masters(&masters_failed, &ctx.state, &ctx.managers, &ctx.registry, false);

    // 4. Decide which tunnels to restart: jump master failed OR ssh -L child
    //    is dead/missing.
    let to_restart: Vec<String> = alive_tunnels
        .iter()
        .filter(|(name, jump)| {
            let jump_failed = jump
                .as_deref()
                .map(|j| masters_failed.iter().any(|m| m == j))
                .unwrap_or(false);
            let child_dead = !matches!(ctx.runtime.child_alive(name), Some(true));
            jump_failed || child_dead
        })
        .map(|(name, _)| name.clone())
        .collect();

    // 5. Reset each to-restart tunnel to Idle but KEEP wants_alive = true so the
    //    always-on maintenance loop revives it once a ready jump master exists.
    {
        let mut guard = crate::lock_state(&ctx.state);
        for name in &to_restart {
            if let Some(t) = guard.tunnels.iter_mut().find(|t| &t.name == name) {
                t.status = TunnelStatus::Idle;
                t.active_jump = None;
                // NOTE: intentionally do NOT touch wants_alive — leaving it true
                // is what hands the restart off to the auto-recovery loop.
                t.last_msg = "wake_recover: restarting".into();
            }
        }
    }

    // 6. Off-lock: kill the dead/stale ssh -L children so the maintenance loop
    //    starts from a clean slate.
    for name in &to_restart {
        ctx.runtime.kill_child(name);
    }

    log::info!(
        "wake_recover: {} tunnels restarting, {} kept",
        to_restart.len(),
        alive_tunnels.len().saturating_sub(to_restart.len())
    );

    Ok(json!({ "tunnels_restarting": to_restart, "coalesced": false }))
}

// ---------------------------------------------------------------------------
// subscribe_events (called inline in the connection loop)
// ---------------------------------------------------------------------------

/// Returns the `subscribed: true` ack.  The actual subscriber wiring is done
/// in `server.rs` before this is called.
pub fn subscribe_events_ack() -> Value {
    json!({ "subscribed": true })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::DaemonCtx;
    use crate::managers::HostManagers;
    use crate::tunnel_runtime::TunnelRuntime;
    use crate::workers::OtpRegistry;
    use a2fa_core::engine::State;
    use a2fa_core::model::{Tunnel, TunnelStatus};
    use std::sync::{Arc, Mutex};

    fn ctx_with_state(state: Arc<Mutex<State>>) -> DaemonCtx {
        DaemonCtx {
            state,
            managers: HostManagers::new(),
            registry: OtpRegistry::new(),
            runtime: TunnelRuntime::new(),
            wake_recover_guard: WakeRecoverGuard::new(),
            post_connect_running: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }

    fn alive_tunnel(name: &str) -> Tunnel {
        Tunnel {
            name: name.into(),
            local_port: 8888,
            remote_port: 8888,
            jump_candidates: None,
            last_node: None,
            last_user: None,
            auto_start: false,
            post_connect_cmd: None,
            tags: vec![],
            url_path: None,
            wants_alive: true,
            status: TunnelStatus::Alive,
            active_jump: Some("k6".into()),
            last_msg: "OK".into(),
            last_alive_at: 0.0,
            total_uptime_sec: 0.0,
            connect_count: 0,
            fail_count: 0,
        }
    }

    #[test]
    fn reset_all_empty_state() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let ctx = ctx_with_state(state);
        let v = reset_all(&ctx, &json!({})).unwrap();
        assert_eq!(v["tunnels_stopped"], 0);
        assert_eq!(v["masters_rebuilt"], 0);
    }

    #[test]
    fn wake_recover_empty_state() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let ctx = ctx_with_state(state);
        let v = wake_recover(&ctx, &json!({})).unwrap();
        assert_eq!(v["tunnels_restarting"].as_array().unwrap().len(), 0);
    }

    /// reset_all on a state with one Alive tunnel and no active hosts marks the
    /// tunnel Idle (kill_child is a safe no-op; masters_rebuilt is 0).
    #[test]
    fn reset_all_marks_alive_tunnel_idle() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![alive_tunnel("nb")])));
        let ctx = ctx_with_state(Arc::clone(&state));
        let v = reset_all(&ctx, &json!({})).unwrap();
        assert_eq!(v["tunnels_stopped"], 1);
        assert_eq!(v["masters_rebuilt"], 0);

        let guard = crate::lock_state(&state);
        let t = &guard.tunnels[0];
        assert_eq!(t.status, TunnelStatus::Idle);
        assert!(!t.wants_alive);
        assert!(t.active_jump.is_none());
    }

    /// A second claim while the first is still held (in-flight) returns None;
    /// after the first claim drops, the in-flight flag is clear again — but the
    /// debounce window now blocks an immediate re-claim.
    #[test]
    fn wake_recover_guard_coalesces_concurrent_and_debounces() {
        let guard = WakeRecoverGuard::new();

        let first = guard.try_claim().expect("first claim should win");
        // Concurrent caller while first is in flight: coalesced (no run).
        assert!(
            guard.try_claim().is_none(),
            "second concurrent claim must coalesce to None"
        );

        // First run completes; Drop stamps last_completed + clears in_flight.
        drop(first);

        // A wake arriving immediately after completion is inside the debounce
        // window, so it is still coalesced (no fresh probe loop).
        assert!(
            guard.try_claim().is_none(),
            "claim within the debounce window must coalesce to None"
        );
    }

    /// Once the debounce window has elapsed, a fresh claim succeeds again.
    #[test]
    fn wake_recover_guard_allows_after_window_elapsed() {
        let guard = WakeRecoverGuard::new();
        drop(guard.try_claim().expect("first claim should win"));
        // Simulate the previous completion being older than the min interval.
        guard.last_completed.store(
            WakeRecoverGuard::now_secs()
                .saturating_sub(WAKE_RECOVER_MIN_INTERVAL_SECS + 1),
            Ordering::SeqCst,
        );
        assert!(
            guard.try_claim().is_some(),
            "claim after the debounce window must succeed"
        );
    }

    /// wake_recover on an empty state reports it ran (not coalesced); a second
    /// immediate call coalesces (the daemon-side authoritative guard).
    #[test]
    fn wake_recover_second_immediate_call_coalesces() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let ctx = ctx_with_state(state);
        let first = wake_recover(&ctx, &json!({})).unwrap();
        assert_eq!(first["coalesced"], false);
        let second = wake_recover(&ctx, &json!({})).unwrap();
        assert_eq!(second["coalesced"], true);
    }

    #[test]
    fn log_tail_missing_file_returns_empty() {
        let _state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        // Use a definitely nonexistent path (log_tail uses the real daemon log;
        // test it directly via tail_file instead).
        let v = tail_file("/nonexistent/path/xyz.log", 10).unwrap();
        assert!(v.is_empty());
    }
}
