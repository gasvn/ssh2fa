//! IPC handlers for system-level methods.
//!
//! Methods: log_tail, wake_recover, reset_all, subscribe_events.
//!
//! Parity: `Auto2FADaemon.handle_request` in daemon.py.

use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

use a2fa_core::engine::State;
use a2fa_core::error::{Error, Result};
use a2fa_core::model::TunnelStatus;
use serde_json::{json, Value};

use crate::managers::{self, active_host_names};

// ---------------------------------------------------------------------------
// log_tail
// ---------------------------------------------------------------------------

/// Return the last `n` lines of `/tmp/auto2fa_daemon.log`.
///
/// Uses a backwards block-read to stay cheap on large files
/// (mirrors `_tail_file` in daemon.py).
pub fn log_tail(_state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let n = params
        .get("lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(200) as usize;

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
        let mut guard = ctx.state.lock().unwrap();
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
    let masters_rebuilt = managers::rebuild_masters(
        &active_host_names(&ctx.state),
        &ctx.state,
        &ctx.managers,
        &ctx.registry,
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
/// The State lock is taken only for brief snapshots and field updates; the
/// 5s master probes run inside `HostManagers` (off both locks via `with_pool`,
/// which only holds the map lock for the duration of the in-memory predicate),
/// and master rebuilds + child kills run off-lock as in `reset_all`.
pub fn wake_recover(ctx: &crate::dispatch::DaemonCtx, _params: &Value) -> Result<Value> {
    // 1. Snapshot the tunnels that were alive at wake time (name, active_jump).
    let alive_tunnels: Vec<(String, Option<String>)> = {
        let guard = ctx.state.lock().unwrap();
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

    // 2. Probe each active host's master (off State lock).  `active_master_ready`
    //    runs `ssh -O check` with a 5s timeout.
    let active_hosts = active_host_names(&ctx.state);
    let masters_failed: Vec<String> = active_hosts
        .iter()
        .filter(|host| {
            let ready = ctx
                .managers
                .with_pool(host, |p| p.active_master_ready())
                .unwrap_or(false);
            !ready
        })
        .cloned()
        .collect();

    log::info!(
        "wake_recover: {} tunnels alive at wake, {} of {} masters failed",
        alive_tunnels.len(),
        masters_failed.len(),
        active_hosts.len()
    );

    // 3. Rebuild the masters that failed (background threads).
    managers::rebuild_masters(&masters_failed, &ctx.state, &ctx.managers, &ctx.registry);

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
        let mut guard = ctx.state.lock().unwrap();
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

    Ok(json!({ "tunnels_restarting": to_restart }))
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

        let guard = state.lock().unwrap();
        let t = &guard.tunnels[0];
        assert_eq!(t.status, TunnelStatus::Idle);
        assert!(!t.wants_alive);
        assert!(t.active_jump.is_none());
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
