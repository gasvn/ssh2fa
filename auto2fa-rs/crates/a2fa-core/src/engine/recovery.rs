//! Wake recovery and full-reset helpers — mirrors `_wake_recover` and
//! `_reset_all` in `daemon.py`.
//!
//! # Design notes
//!
//! Both functions snapshot the tunnel list by **cloning** (`Vec::clone()`) or
//! collecting names **before** iterating. This is the Rust equivalent of the
//! Python pattern:
//! ```python
//! alive_tunnels = [(name, ts.active_jump)
//!                  for name, ts in list(self.tunnel_mgr.tunnels.items())
//!                  if ts.status in ("alive", "starting", "stale")]
//! ```
//! Snapshotting first avoids "changed size during iteration" panics if a tunnel
//! is added or removed by another thread mid-loop.
//!
//! # Deferred: live SSH operations
//!
//! The actual `ssh -O check` probes and `start_forward` / `stop_slot` calls
//! that happen in the Python versions are **structurally present** (as TODO
//! stubs) but return early without blocking I/O. They will be wired to the
//! real `crate::ssh` and `crate::tunnels` helpers in a follow-up integration
//! task. All the book-keeping logic (snapshot, decide which tunnels need work,
//! schedule retries) is already in place.
//!
//! # Discovery via master ControlPath
//!
//! When the engine triggers node discovery it MUST reuse the host's existing
//! master ControlPath rather than opening a fresh SSH connection (which would
//! re-trigger 2FA). The call site should pass the ControlPath string to
//! `crate::tunnels::discover_nodes_via_master(jump, control_path)`.
//!
//! In the current stub that path is documented on `WakeRecoverResult` and the
//! caller is expected to supply it. A future integration task will wire this
//! to `crate::ssh::control::active_symlink_path(host)`.

use std::sync::Mutex;

use log::info;

use crate::engine::schedule::WAKE_RETRY_DELAYS;
use crate::engine::State;
use crate::model::TunnelStatus;

// ---------------------------------------------------------------------------
// Return types
// ---------------------------------------------------------------------------

/// Summary of a `reset_all` operation.
#[derive(Debug, Clone)]
pub struct ResetResult {
    /// Number of tunnels that were stopped.
    pub tunnels_stopped: usize,
    /// Number of host masters that were (or would be) rebuilt.
    pub masters_rebuilt: usize,
}

/// Summary of a `wake_recover` operation.
#[derive(Debug, Clone)]
pub struct WakeRecoverResult {
    /// Names of tunnels that were stopped and scheduled for restart.
    pub tunnels_restarting: Vec<String>,
    /// Names of hosts whose master failed the liveness probe.
    pub masters_failed: Vec<String>,
}

// ---------------------------------------------------------------------------
// reset_all
// ---------------------------------------------------------------------------

/// Stop every active tunnel and signal that all enabled masters need rebuild.
///
/// Mirrors `Auto2FADaemon._reset_all` in `daemon.py`.
///
/// # Lock discipline
/// Takes `state` briefly to snapshot tunnel names + active status, then drops
/// the lock before any blocking work. Results are written back under a fresh
/// lock acquisition.
///
/// # Deferred
/// Actual `stop_forward` / `force_master_rebuild` calls are stubbed; the
/// function marks tunnel statuses as `Idle` and returns counts. Live I/O will
/// be wired in the integration task.
pub fn reset_all(state: &Mutex<State>) -> ResetResult {
    // 1. Snapshot under lock — note which tunnels are currently active.
    let (active_names, active_host_count): (Vec<String>, usize) = {
        let guard = state.lock().unwrap();
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
        let host_count = guard.hosts.iter().filter(|h| h.active).count();
        (names, host_count)
    };

    info!(
        "reset_all: stopping {} tunnels, rebuilding {} masters",
        active_names.len(),
        active_host_count
    );

    // 2. Off-lock: stub for stop_forward + force_master_rebuild.
    // TODO(integration): call crate::tunnels::stop_forward and
    //     crate::ssh::master::stop_all / start_master for each active host.

    // 3. Re-lock to update state.
    {
        let mut guard = state.lock().unwrap();
        for name in &active_names {
            if let Some(t) = guard.tunnels.iter_mut().find(|t| &t.name == name) {
                t.status = TunnelStatus::Idle;
                t.last_msg = "Stopped (reset_all)".into();
                t.active_jump = None;
            }
        }
    }

    ResetResult {
        tunnels_stopped: active_names.len(),
        masters_rebuilt: active_host_count,
    }
}

// ---------------------------------------------------------------------------
// wake_recover
// ---------------------------------------------------------------------------

/// Restore connectivity after Mac wake / network change.
///
/// Mirrors `Auto2FADaemon._wake_recover` in `daemon.py`:
/// 1. Snapshot alive tunnels (with their active_jump) BEFORE modifying state.
/// 2. Probe each enabled host's active master — record which failed.
/// 3. Stop tunnels whose master failed OR whose ssh -L child is dead.
/// 4. Schedule a backoff-retried restart for each stopped tunnel.
///
/// # Deferred
/// The actual `ssh -O check` probe, `stop_forward`, `start_forward`, and the
/// async retry loop are stubbed. The book-keeping (snapshot, decide, report)
/// is complete. Full I/O will be wired in the integration task.
pub fn wake_recover(state: &Mutex<State>) -> WakeRecoverResult {
    // 1. Snapshot tunnels that were alive before we do anything.
    let alive_tunnels: Vec<(String, Option<String>)> = {
        let guard = state.lock().unwrap();
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

    info!(
        "wake_recover: {} tunnels were alive at wake time",
        alive_tunnels.len()
    );

    // 2. Probe masters (off-lock).
    // TODO(integration): for each active host, run:
    //     let path = crate::ssh::control::active_symlink_path(&host.host);
    //     let ok = std::process::Command::new("ssh")
    //         .args(["-o", &format!("ControlPath={}", path.display()), &host.host, "true"])
    //         .timeout(Duration::from_secs(5))
    //         .status()
    //         .map(|s| s.success()).unwrap_or(false);
    //     if !ok { masters_failed.insert(host.host.clone()); rebuild_master(...); }
    //
    // Discovery NOTE: when later triggering NodeDiscovery from this path,
    //     pass the ControlPath to avoid re-triggering 2FA:
    //     crate::tunnels::discover_nodes_via_master(&jump, &control_path)
    //     The control_path is obtained from:
    //     crate::ssh::control::active_symlink_path(&host_name)

    let masters_failed: Vec<String> = {
        // Stub: no live probe yet — assume all masters survived.
        Vec::new()
    };

    // 3. Decide which tunnels to stop + restart.
    let to_restart: Vec<String> = {
        // TODO(integration): also check if ssh -L child is dead (even if
        //     master survived). For now, only restart tunnels on a failed master.
        alive_tunnels
            .iter()
            .filter(|(_, jump)| {
                jump.as_deref()
                    .map(|j| masters_failed.contains(&j.to_string()))
                    .unwrap_or(false)
            })
            .map(|(name, _)| name.clone())
            .collect()
    };

    // Stop tunnels that need restart (off-lock actual stop; for now just mark).
    {
        let mut guard = state.lock().unwrap();
        for name in &to_restart {
            if let Some(t) = guard.tunnels.iter_mut().find(|t| &t.name == name) {
                t.status = TunnelStatus::Idle;
                t.last_msg = "wake_recover: master failed — restarting".into();
                t.active_jump = None;
            }
        }
    }

    info!(
        "wake_recover: {} tunnels need restart, {} kept",
        to_restart.len(),
        alive_tunnels.len().saturating_sub(to_restart.len())
    );

    // 4. Retry schedule.
    // TODO(integration): schedule async retries with WAKE_RETRY_DELAYS.
    // The retry logic is:
    //   for delay in WAKE_RETRY_DELAYS {
    //       sleep(delay);
    //       for name in still_idle:
    //           if tunnel.status != Alive { try start_forward(...); }
    //   }
    // This will be driven by the tick loop or a dedicated recovery thread.
    if !to_restart.is_empty() {
        info!(
            "wake_recover: retry schedule: {:?}",
            WAKE_RETRY_DELAYS
                .iter()
                .map(|d| d.as_secs())
                .collect::<Vec<_>>()
        );
    }

    WakeRecoverResult {
        tunnels_restarting: to_restart,
        masters_failed,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::State;
    use crate::model::{Tunnel, TunnelStatus};
    use std::sync::Mutex;

    fn make_tunnel(name: &str, status: TunnelStatus, jump: Option<&str>) -> Tunnel {
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
            status,
            active_jump: jump.map(|s| s.to_owned()),
            last_msg: "OK".into(),
            last_alive_at: 0.0,
            total_uptime_sec: 0.0,
            connect_count: 0,
            fail_count: 0,
        }
    }

    #[test]
    fn reset_all_stops_active_tunnels() {
        let state = Mutex::new(State::with_tunnels(vec![
            make_tunnel("nb",   TunnelStatus::Alive,    Some("k6")),
            make_tunnel("db",   TunnelStatus::Starting, None),
            make_tunnel("idle", TunnelStatus::Idle,     None),
        ]));

        let result = reset_all(&state);

        assert_eq!(result.tunnels_stopped, 2); // alive + starting
        let guard = state.lock().unwrap();
        let alive = guard.tunnels.iter().find(|t| t.name == "nb").unwrap();
        assert_eq!(alive.status, TunnelStatus::Idle);
    }

    #[test]
    fn reset_all_on_empty_state() {
        let state = Mutex::new(State::with_tunnels(vec![]));
        let result = reset_all(&state);
        assert_eq!(result.tunnels_stopped, 0);
    }

    #[test]
    fn wake_recover_snapshots_before_modification() {
        // Tunnels alive at wake time; no masters actually fail in the stub.
        let state = Mutex::new(State::with_tunnels(vec![
            make_tunnel("nb", TunnelStatus::Alive, Some("k6")),
            make_tunnel("db", TunnelStatus::Stale, Some("k8")),
        ]));

        let result = wake_recover(&state);

        // With no failed masters, nothing is restarted.
        assert_eq!(result.tunnels_restarting.len(), 0);
        assert_eq!(result.masters_failed.len(), 0);
        // Tunnels are unchanged.
        let guard = state.lock().unwrap();
        assert_eq!(guard.tunnels[0].status, TunnelStatus::Alive);
    }

    #[test]
    fn wake_recover_idle_tunnels_are_ignored() {
        let state = Mutex::new(State::with_tunnels(vec![
            make_tunnel("idle1", TunnelStatus::Idle,   None),
            make_tunnel("idle2", TunnelStatus::Failed, None),
        ]));
        let result = wake_recover(&state);
        assert_eq!(result.tunnels_restarting.len(), 0);
    }
}
