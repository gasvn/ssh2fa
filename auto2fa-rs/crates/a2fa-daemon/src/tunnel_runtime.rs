//! Runtime-only per-tunnel state: child process registry and maintenance
//! counters.
//!
//! Nothing here is persisted — on daemon restart the registry starts empty and
//! the maintenance counters start at zero.  Persisted tunnel state (wants_alive,
//! last_node, etc.) lives in `a2fa_core::model::Tunnel` / tunnels.json.
//!
//! # Child registry
//!
//! Maps tunnel name → the live `std::process::Child` for the `ssh -L` process.
//!
//! * `spawn_tunnel_start` (in `workers.rs`) stores the child here after a
//!   successful port probe.
//! * `stop_tunnel_child` (called from the stop / remove handlers) looks the
//!   child up and SIGKILLs it via `stop_forward`.
//! * The maintenance tick calls `try_wait` on each child without removing it
//!   (to detect child-died) and removes + kills it when the tunnel is stopped.
//!
//! # Runtime counters
//!
//! `TunnelRtState` holds the two throttle counters that the Python daemon
//! tracked on `TunnelState` but that the Rust model deliberately omits from
//! the persisted schema:
//!
//! * `last_recovery_attempt_ts` — Unix epoch (f64).  Auto-recovery is throttled
//!   to one attempt per 15 s.
//! * `last_squeue_check_ts` — Unix epoch (f64).  Squeue discovery is throttled
//!   to one check per 30 s.
//! * `consecutive_squeue_misses` — u32.  Stale threshold is 2 misses.
//! * `alive_since` — Option<f64>.  Set when the tunnel enters Alive; folded
//!   into `total_uptime_sec` when it leaves Alive.

use std::collections::HashMap;
use std::process::Child;
use std::sync::{Arc, Mutex};

use a2fa_core::tunnels::forward::stop_forward;
use log::{info, warn};

// ---------------------------------------------------------------------------
// Per-tunnel runtime counters
// ---------------------------------------------------------------------------

/// Runtime-only fields for a single tunnel (not persisted to tunnels.json).
#[derive(Debug)]
pub struct TunnelRtState {
    /// Unix timestamp of the last auto-recovery attempt.
    /// Throttles recovery to once per [`AUTO_RECOVERY_INTERVAL_SEC`].
    pub last_recovery_attempt_ts: f64,

    /// Unix timestamp of the last squeue discovery check for this tunnel.
    /// Throttles squeue to once per [`SQUEUE_INTERVAL_SEC`].
    pub last_squeue_check_ts: f64,

    /// How many consecutive squeue checks have not found this tunnel's node.
    pub consecutive_squeue_misses: u32,

    /// When did this tunnel last enter the Alive status?  Used to accumulate
    /// uptime when it leaves Alive.  `None` = not currently alive.
    pub alive_since: Option<f64>,
}

impl Default for TunnelRtState {
    fn default() -> Self {
        Self {
            last_recovery_attempt_ts: 0.0,
            last_squeue_check_ts: 0.0,
            consecutive_squeue_misses: 0,
            alive_since: None,
        }
    }
}

// ---------------------------------------------------------------------------
// TunnelRuntime — the daemon-global registry
// ---------------------------------------------------------------------------

/// Auto-recovery throttle (mirrors Python `AUTO_RECOVERY_INTERVAL_SEC = 15`).
pub const AUTO_RECOVERY_INTERVAL_SEC: f64 = 15.0;

/// Squeue check interval (mirrors Python `DISCOVERY_INTERVAL_SEC = 30`).
pub const SQUEUE_INTERVAL_SEC: f64 = 30.0;

/// Consecutive squeue misses before a tunnel is marked stale
/// (mirrors Python `STALE_MISS_THRESHOLD = 2`).
pub const STALE_MISS_THRESHOLD: u32 = 2;

/// Boot grace period before auto-start fires (mirrors Python `now - startup_ts >= 3.0`).
pub const BOOT_GRACE_SEC: f64 = 3.0;

/// Daemon-global registry: child handles + per-tunnel runtime counters.
///
/// Both maps are keyed by tunnel name and protected by a single `Mutex`.
/// All critical sections are brief (hash-map lookups, field reads/writes).
/// No blocking I/O is ever performed while holding this lock.
#[derive(Default)]
pub struct TunnelRuntime {
    inner: Mutex<RuntimeInner>,
}

#[derive(Default)]
struct RuntimeInner {
    /// Live `ssh -L` child processes, keyed by tunnel name.
    children: HashMap<String, Child>,
    /// Per-tunnel throttle / uptime counters, keyed by tunnel name.
    rt: HashMap<String, TunnelRtState>,
    /// Unix timestamp when the daemon started.  `0.0` = not yet set.
    pub startup_ts: f64,
    /// Whether the one-shot boot auto-start has already fired.
    pub auto_started: bool,
}

impl TunnelRuntime {
    /// Create a new, empty runtime wrapped in an `Arc`.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record the daemon startup time.  Must be called once from `server::run`.
    pub fn set_startup_ts(&self, ts: f64) {
        self.inner.lock().unwrap().startup_ts = ts;
    }

    // ---- Child registry -----------------------------------------------

    /// Store a child handle after a successful tunnel start.
    pub fn store_child(&self, name: &str, child: Child) {
        let mut inner = self.inner.lock().unwrap();
        // If a previous child is still there (shouldn't normally happen), kill it.
        if let Some(old) = inner.children.remove(name) {
            warn!("[tunnel:{name}] store_child: evicting stale child handle");
            stop_forward(old);
        }
        inner.children.insert(name.to_owned(), child);
        info!("[tunnel:{name}] child handle stored in registry");
    }

    /// Remove and return the child for `name` (if any).
    ///
    /// Returns `None` when no child is registered (tunnel never started, or
    /// already reaped).
    pub fn take_child(&self, name: &str) -> Option<Child> {
        self.inner.lock().unwrap().children.remove(name)
    }

    /// Kill the child for `name` (if any) via `stop_forward` (SIGKILL + wait).
    ///
    /// Idempotent — silently does nothing when no child is registered.
    pub fn kill_child(&self, name: &str) {
        if let Some(child) = self.take_child(name) {
            info!("[tunnel:{name}] kill_child: sending SIGKILL");
            stop_forward(child);
        }
    }

    /// Call `try_wait` on the child for `name` without removing it from the
    /// registry.
    ///
    /// Returns:
    /// * `None`  — no child registered (tunnel was never started).
    /// * `Some(true)`  — child is still running.
    /// * `Some(false)` — child has exited (reaped from the process table).
    pub fn child_alive(&self, name: &str) -> Option<bool> {
        let mut inner = self.inner.lock().unwrap();
        let child = inner.children.get_mut(name)?;
        match child.try_wait() {
            Ok(None) => Some(true),   // still running
            Ok(Some(_)) => Some(false), // exited
            Err(_) => Some(false),    // can't determine — treat as dead
        }
    }

    // ---- Runtime counters ----------------------------------------------

    /// Borrow the runtime state for `name` mutably via a closure.
    ///
    /// Creates a default `TunnelRtState` if none exists yet.
    pub fn with_rt_mut<R>(&self, name: &str, f: impl FnOnce(&mut TunnelRtState) -> R) -> R {
        let mut inner = self.inner.lock().unwrap();
        let rt = inner.rt.entry(name.to_owned()).or_default();
        f(rt)
    }

    /// Read the runtime state for `name` via a closure.
    ///
    /// Returns `None` if no state exists yet for this tunnel.
    pub fn with_rt<R>(&self, name: &str, f: impl FnOnce(&TunnelRtState) -> R) -> Option<R> {
        let inner = self.inner.lock().unwrap();
        inner.rt.get(name).map(f)
    }

    /// Remove both the child handle and the runtime state for `name`.
    ///
    /// Called when a tunnel is removed from the daemon entirely.
    pub fn remove(&self, name: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(child) = inner.children.remove(name) {
            // Kill off-lock isn't possible here without a two-phase approach,
            // but stop_forward (SIGKILL) is fast enough that holding the lock
            // briefly is acceptable for the remove path (rare operation).
            stop_forward(child);
        }
        inner.rt.remove(name);
    }

    // ---- Boot auto-start bookkeeping ----------------------------------

    /// Returns `(startup_ts, auto_started)`.
    pub fn boot_state(&self) -> (f64, bool) {
        let inner = self.inner.lock().unwrap();
        (inner.startup_ts, inner.auto_started)
    }

    /// Mark the one-shot boot auto-start as completed.
    pub fn mark_auto_started(&self) {
        self.inner.lock().unwrap().auto_started = true;
    }
}

// ---------------------------------------------------------------------------
// Decision layer — pure functions, no I/O
// ---------------------------------------------------------------------------

/// What the maintenance tick should do for one tunnel in one pass.
///
/// This is a pure enum — it carries no side effects.  The caller performs
/// the actual ssh / state mutations after checking the variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelAction {
    /// Nothing to do this tick.
    Skip,
    /// Tunnel wants to be alive but is idle/failed/stale/port_busy, and the
    /// recovery throttle has elapsed → attempt a restart.
    Recover,
    /// Tunnel shows Alive in State but the child has exited or the port is
    /// no longer bound → stop (non-user) then recover.
    StopDead,
    /// Tunnel shows Alive but its active jump host is now disabled → stop
    /// (user-initiated, no auto-recover).
    StopDisabledJump,
    /// Squeue check is due for this tunnel (status == Alive, throttle elapsed).
    SqueueCheck,
}

/// The persisted status values that indicate a tunnel is not currently alive.
/// Mirrors Python's `IDLE_STATUSES` used in `tick()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelStatusKind {
    Idle,
    Failed,
    Stale,
    PortBusy,
    Starting,
    Alive,
}

/// Pure decision function: given the current tunnel state and runtime
/// observations, return the `TunnelAction` for this tick.
///
/// # Arguments
///
/// * `status`              — current persisted status of the tunnel.
/// * `wants_alive`         — persisted desired-alive flag.
/// * `child_alive` — `Some(true/false)` from `try_wait`; `None` if no
///   child is registered.
/// * `port_bound` — `!port_available(local_port)` (true ⟺ port is in
///   use, i.e. the forward is up).
/// * `jump_active` — whether the tunnel's `active_jump` host has
///   `active == true` in State (None = no active_jump).
/// * `last_recovery_ts`    — Unix epoch of the last recovery attempt.
/// * `last_squeue_ts`      — Unix epoch of the last squeue check.
/// * `now`                 — current Unix epoch (f64).
#[allow(clippy::too_many_arguments)]
pub fn tunnel_action(
    status: TunnelStatusKind,
    wants_alive: bool,
    child_alive: Option<bool>,
    port_bound: bool,
    jump_active: Option<bool>,
    last_recovery_ts: f64,
    last_squeue_ts: f64,
    now: f64,
) -> TunnelAction {
    use TunnelStatusKind::*;

    match status {
        // --- Tunnel wants to be alive but isn't ---
        Idle | Failed | Stale | PortBusy if wants_alive => {
            if now - last_recovery_ts >= AUTO_RECOVERY_INTERVAL_SEC {
                TunnelAction::Recover
            } else {
                TunnelAction::Skip
            }
        }

        // --- Starting: wait for the worker thread ---
        Starting => TunnelAction::Skip,

        // --- Nothing wanted ---
        Idle | Failed | Stale | PortBusy => TunnelAction::Skip,

        // --- Alive: health checks ---
        Alive => {
            // Case 1: child died or port no longer bound (ghost-alive).
            let child_dead = match child_alive {
                None => false,          // no child in registry → assume external handle
                Some(alive) => !alive,
            };
            if child_dead || !port_bound {
                return TunnelAction::StopDead;
            }

            // Case 2: jump host explicitly disabled.
            if jump_active == Some(false) {
                return TunnelAction::StopDisabledJump;
            }

            // Case 3: squeue check due.
            if now - last_squeue_ts >= SQUEUE_INTERVAL_SEC {
                return TunnelAction::SqueueCheck;
            }

            TunnelAction::Skip
        }
    }
}

/// Whether a tunnel should be auto-started at boot.
///
/// Mirrors Python's `tick()` boot logic:
///   `want = ts.auto_start or ts.wants_alive`
///   `if want and ts.last_node is not None: start(name)`
pub fn should_autostart(auto_start: bool, wants_alive: bool, last_node: Option<&str>) -> bool {
    (auto_start || wants_alive) && last_node.is_some()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: a "now" timestamp far enough from 0 that throttles all pass.
    const NOW: f64 = 1_700_000_000.0;
    const OLD: f64 = 0.0; // treated as "long ago"

    // ---- tunnel_action: Case 0 — auto-recovery --------------------------

    #[test]
    fn wants_alive_idle_throttle_elapsed_gives_recover() {
        let action = tunnel_action(
            TunnelStatusKind::Idle,
            /*wants_alive=*/ true,
            None,
            /*port_bound=*/ false,
            None,
            /*last_recovery_ts=*/ OLD,
            OLD,
            NOW,
        );
        assert_eq!(action, TunnelAction::Recover);
    }

    #[test]
    fn wants_alive_failed_throttle_elapsed_gives_recover() {
        let action = tunnel_action(
            TunnelStatusKind::Failed,
            true,
            None,
            false,
            None,
            OLD,
            OLD,
            NOW,
        );
        assert_eq!(action, TunnelAction::Recover);
    }

    #[test]
    fn wants_alive_stale_throttle_elapsed_gives_recover() {
        let action = tunnel_action(
            TunnelStatusKind::Stale,
            true,
            None,
            false,
            None,
            OLD,
            OLD,
            NOW,
        );
        assert_eq!(action, TunnelAction::Recover);
    }

    #[test]
    fn wants_alive_port_busy_throttle_elapsed_gives_recover() {
        let action = tunnel_action(
            TunnelStatusKind::PortBusy,
            true,
            None,
            false,
            None,
            OLD,
            OLD,
            NOW,
        );
        assert_eq!(action, TunnelAction::Recover);
    }

    #[test]
    fn wants_alive_idle_throttle_not_elapsed_gives_skip() {
        let recent = NOW - 5.0; // only 5 s ago, threshold is 15 s
        let action = tunnel_action(
            TunnelStatusKind::Idle,
            true,
            None,
            false,
            None,
            /*last_recovery_ts=*/ recent,
            OLD,
            NOW,
        );
        assert_eq!(action, TunnelAction::Skip);
    }

    #[test]
    fn no_wants_alive_idle_gives_skip() {
        let action = tunnel_action(
            TunnelStatusKind::Idle,
            /*wants_alive=*/ false,
            None,
            false,
            None,
            OLD,
            OLD,
            NOW,
        );
        assert_eq!(action, TunnelAction::Skip);
    }

    #[test]
    fn starting_always_skip() {
        let action = tunnel_action(
            TunnelStatusKind::Starting,
            true,
            None,
            false,
            None,
            OLD,
            OLD,
            NOW,
        );
        assert_eq!(action, TunnelAction::Skip);
    }

    // ---- tunnel_action: Case 1 — child died / ghost-alive ---------------

    #[test]
    fn alive_child_dead_gives_stop_dead() {
        let action = tunnel_action(
            TunnelStatusKind::Alive,
            true,
            /*child_alive=*/ Some(false),
            /*port_bound=*/ true,
            Some(true),
            OLD,
            OLD,
            NOW,
        );
        assert_eq!(action, TunnelAction::StopDead);
    }

    #[test]
    fn alive_port_not_bound_ghost_gives_stop_dead() {
        let action = tunnel_action(
            TunnelStatusKind::Alive,
            true,
            /*child_alive=*/ Some(true), // child reports alive but port gone
            /*port_bound=*/ false,
            Some(true),
            OLD,
            OLD,
            NOW,
        );
        assert_eq!(action, TunnelAction::StopDead);
    }

    // ---- tunnel_action: Case 2 — disabled jump --------------------------

    #[test]
    fn alive_jump_inactive_gives_stop_disabled_jump() {
        let action = tunnel_action(
            TunnelStatusKind::Alive,
            true,
            Some(true),
            true,
            /*jump_active=*/ Some(false),
            OLD,
            OLD,
            NOW,
        );
        assert_eq!(action, TunnelAction::StopDisabledJump);
    }

    #[test]
    fn alive_jump_active_and_healthy_and_squeue_due_gives_squeue_check() {
        let action = tunnel_action(
            TunnelStatusKind::Alive,
            true,
            Some(true),
            true,
            Some(true),
            OLD,
            /*last_squeue_ts=*/ OLD, // throttle elapsed
            NOW,
        );
        assert_eq!(action, TunnelAction::SqueueCheck);
    }

    #[test]
    fn alive_all_healthy_squeue_not_due_gives_skip() {
        let recent = NOW - 10.0; // only 10 s ago, threshold is 30 s
        let action = tunnel_action(
            TunnelStatusKind::Alive,
            true,
            Some(true),
            true,
            Some(true),
            OLD,
            /*last_squeue_ts=*/ recent,
            NOW,
        );
        assert_eq!(action, TunnelAction::Skip);
    }

    // ---- should_autostart -----------------------------------------------

    #[test]
    fn autostart_flag_with_node_gives_true() {
        assert!(should_autostart(true, false, Some("holygpu01")));
    }

    #[test]
    fn wants_alive_with_node_gives_true() {
        assert!(should_autostart(false, true, Some("holygpu01")));
    }

    #[test]
    fn autostart_flag_without_node_gives_false() {
        assert!(!should_autostart(true, false, None));
    }

    #[test]
    fn wants_alive_without_node_gives_false() {
        assert!(!should_autostart(false, true, None));
    }

    #[test]
    fn neither_flag_set_gives_false() {
        assert!(!should_autostart(false, false, Some("holygpu01")));
    }

    // ---- Child registry (unit-level: store / take / kill) ---------------

    #[test]
    fn registry_store_and_take() {
        use std::process::Command;
        let rt = TunnelRuntime::new();

        // Spawn a real process (sleep is available on macOS/Linux).
        let child = Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn sleep");

        rt.store_child("nb", child);

        // Child should be registered.
        assert!(rt.child_alive("nb").is_some());

        // Take removes it.
        let child = rt.take_child("nb").expect("child should be registered");
        // After take, it's gone.
        assert!(rt.take_child("nb").is_none());

        // Clean up.
        stop_forward(child);
    }

    #[test]
    fn registry_kill_child() {
        use std::process::Command;
        let rt = TunnelRuntime::new();

        let child = Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn sleep");

        rt.store_child("nb", child);
        rt.kill_child("nb"); // should not panic

        // After kill, no child in registry.
        assert!(rt.take_child("nb").is_none());
    }

    #[test]
    fn registry_child_alive_for_running_child() {
        use std::process::Command;
        let rt = TunnelRuntime::new();

        let child = Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn sleep");

        rt.store_child("nb", child);
        // Still running → child_alive returns Some(true).
        assert_eq!(rt.child_alive("nb"), Some(true));

        rt.kill_child("nb");
    }

    #[test]
    fn registry_child_alive_none_when_not_registered() {
        let rt = TunnelRuntime::new();
        assert_eq!(rt.child_alive("ghost"), None);
    }

    #[test]
    fn registry_remove_kills_and_cleans_up() {
        use std::process::Command;
        let rt = TunnelRuntime::new();

        let child = Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn sleep");

        rt.store_child("nb", child);
        rt.with_rt_mut("nb", |r| r.consecutive_squeue_misses = 3);

        rt.remove("nb");

        assert!(rt.take_child("nb").is_none());
        assert!(rt.with_rt("nb", |_| ()).is_none());
    }
}
