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

use std::collections::{HashMap, VecDeque};
use std::process::Child;
use std::sync::{Arc, Mutex};

use a2fa_core::tunnels::forward::stop_forward;
use log::{info, warn};

// ---------------------------------------------------------------------------
// Event ring buffer
// ---------------------------------------------------------------------------

/// Maximum number of events retained per tunnel (mirrors Python EVENT_BUFFER_LIMIT = 200).
pub const EVENT_BUFFER_LIMIT: usize = 200;

/// A single status-transition event recorded at runtime.
#[derive(Debug, Clone)]
pub struct TunnelEvent {
    /// Unix timestamp (seconds, floating-point) when this event was recorded.
    pub ts: f64,
    /// Human-readable description of the transition.
    pub msg: String,
}

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

    /// Consecutive short-lived Alive runs (died before
    /// [`TUNNEL_FLAP_MIN_UPTIME_SEC`]). At [`TUNNEL_FLAP_THRESHOLD`] the
    /// StopDead → immediate-recover path backs off instead of respawning.
    pub consecutive_flaps: u32,

    /// Consecutive recovery attempts that FAILED to bring the tunnel up (never
    /// reached Alive). Reset to 0 on a successful connect. At
    /// [`RECOVERY_FAILURE_THRESHOLD`] the tunnel auto-stops (clears
    /// `wants_alive`) instead of retrying forever against a target it can't
    /// reach — see `note_recovery_failure_and_maybe_stop`.
    pub consecutive_recovery_failures: u32,
}

impl Default for TunnelRtState {
    fn default() -> Self {
        Self {
            last_recovery_attempt_ts: 0.0,
            last_squeue_check_ts: 0.0,
            consecutive_squeue_misses: 0,
            alive_since: None,
            consecutive_flaps: 0,
            consecutive_recovery_failures: 0,
        }
    }
}

/// How many consecutive recovery failures before a `wants_alive` tunnel
/// auto-stops (clears `wants_alive`) and notifies the user, instead of retrying
/// forever against an unreachable target.
pub const RECOVERY_FAILURE_THRESHOLD: u32 = 5;

/// Pure decision: should a tunnel auto-stop after this many consecutive recovery
/// failures? Extracted so the threshold logic is unit-tested without the daemon.
pub fn should_auto_stop_after_failures(consecutive_failures: u32, threshold: u32) -> bool {
    consecutive_failures >= threshold
}

/// Flap accounting for the StopDead path. Called when an Alive tunnel is found
/// dead, with the uptime of the run that just ended (`None` = unknown).
///
/// Updates `consecutive_flaps` and sets `last_recovery_attempt_ts` so that:
/// * a one-off drop (or a long stable run) still recovers IMMEDIATELY — the
///   responsive-UX behavior StopDead always had;
/// * a tunnel that keeps dying within [`TUNNEL_FLAP_MIN_UPTIME_SEC`] of each
///   (re)start backs off [`TUNNEL_FLAP_BACKOFF_SEC`] once it hits
///   [`TUNNEL_FLAP_THRESHOLD`], instead of kill+respawn every ~1-2 s forever.
///
/// Returns `true` if the backoff was armed (caller logs it).
pub fn note_stop_dead_flap(rt: &mut TunnelRtState, uptime_sec: Option<f64>, now: f64) -> bool {
    match uptime_sec {
        Some(u) if u < TUNNEL_FLAP_MIN_UPTIME_SEC => rt.consecutive_flaps += 1,
        Some(_) => rt.consecutive_flaps = 0, // long stable run → not a flap
        None => {}                           // unknown uptime → leave counter as-is
    }
    if rt.consecutive_flaps >= TUNNEL_FLAP_THRESHOLD {
        // The Recover gate is `now - last_recovery_attempt_ts >= AUTO_RECOVERY_
        // INTERVAL_SEC`, so writing a timestamp this far ahead delays the next
        // attempt by exactly TUNNEL_FLAP_BACKOFF_SEC.
        rt.last_recovery_attempt_ts = now + TUNNEL_FLAP_BACKOFF_SEC - AUTO_RECOVERY_INTERVAL_SEC;
        true
    } else {
        rt.last_recovery_attempt_ts = 0.0; // immediate recover (one-off drop)
        false
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

/// A tunnel that stays Alive at least this long counts as a STABLE run
/// (clears the flap counter). Dying sooner is a "flap" (start-then-drop).
pub const TUNNEL_FLAP_MIN_UPTIME_SEC: f64 = 30.0;

/// Consecutive flaps before the StopDead → immediate-recover path backs off.
pub const TUNNEL_FLAP_THRESHOLD: u32 = 4;

/// How long a flapping tunnel sits out before the next recovery attempt.
/// Without this, StopDead reset the recovery throttle to 0 unconditionally, so
/// a tunnel that dies right after every (re)start was killed + respawned every
/// ~1-2 s forever — the same churn class as the ssh-master flap backoff.
pub const TUNNEL_FLAP_BACKOFF_SEC: f64 = 60.0;

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
    /// Per-tunnel status-transition event ring buffers, keyed by tunnel name.
    events: HashMap<String, VecDeque<TunnelEvent>>,
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
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).startup_ts = ts;
    }

    // ---- Child registry -----------------------------------------------

    /// Store a child handle after a successful tunnel start.
    ///
    /// Two-phase so we never hold the registry lock across a blocking child
    /// `wait()`: (1) under the lock, REMOVE any stale child into a local and
    /// drop the lock; (2) `stop_forward` (kill+wait) the stale child OFF-lock —
    /// a D-state (wedged NFS/ssh) child would otherwise freeze the whole
    /// maintenance tick, which also locks this registry; (3) re-lock to insert
    /// the new child. Mirrors `kill_child`'s evict-under-lock / kill-off-lock.
    pub fn store_child(&self, name: &str, child: Child) {
        // Phase 1: evict any stale child under the lock, then drop the lock.
        let stale = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.children.remove(name)
        };
        // Phase 2: reap the stale child OFF-lock (kill+wait may block on D-state).
        if let Some(old) = stale {
            warn!("[tunnel:{name}] store_child: evicting stale child handle");
            stop_forward(old);
        }
        // Phase 3: re-lock and insert the new child.
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.children.insert(name.to_owned(), child);
        info!("[tunnel:{name}] child handle stored in registry");
    }

    /// Remove and return the child for `name` (if any).
    ///
    /// Returns `None` when no child is registered (tunnel never started, or
    /// already reaped).
    pub fn take_child(&self, name: &str) -> Option<Child> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).children.remove(name)
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
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let child = inner.children.get_mut(name)?;
        match child.try_wait() {
            Ok(None) => Some(true),   // still running
            Ok(Some(_)) => Some(false), // exited
            // A transient try_wait error (e.g. EINTR) must NOT report "dead" —
            // that force-killed and respawned a perfectly healthy tunnel. Claim
            // alive and let the next tick (1 s) re-check; if the child is truly
            // dead the local-port probe (`!port_bound` → StopDead) is the
            // backstop, so a persistent error can't mask a real death.
            Err(_) => Some(true),
        }
    }

    // ---- Runtime counters ----------------------------------------------

    /// Borrow the runtime state for `name` mutably via a closure.
    ///
    /// Creates a default `TunnelRtState` if none exists yet.
    pub fn with_rt_mut<R>(&self, name: &str, f: impl FnOnce(&mut TunnelRtState) -> R) -> R {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let rt = inner.rt.entry(name.to_owned()).or_default();
        f(rt)
    }

    /// Read the runtime state for `name` via a closure.
    ///
    /// Returns `None` if no state exists yet for this tunnel.
    pub fn with_rt<R>(&self, name: &str, f: impl FnOnce(&TunnelRtState) -> R) -> Option<R> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.rt.get(name).map(f)
    }

    /// Remove both the child handle and the runtime state for `name`.
    ///
    /// Called when a tunnel is removed from the daemon entirely.
    ///
    /// Two-phase like `store_child`: take the child out UNDER the lock, drop the
    /// lock, then `stop_forward` (kill+wait) OFF-lock so a D-state child can't
    /// freeze the maintenance tick (which also locks this registry).
    pub fn remove(&self, name: &str) {
        let child = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let child = inner.children.remove(name);
            inner.rt.remove(name);
            inner.events.remove(name);
            child
        };
        // Reap OFF-lock (SIGKILL + wait may block on a wedged child).
        if let Some(child) = child {
            stop_forward(child);
        }
    }

    /// Re-key every runtime entry (live child, counters, events) from `old` to
    /// `new` on a tunnel rename. Without this, renaming an Alive tunnel left
    /// its `ssh -L` child registered under the OLD name — an orphan forward
    /// nothing tracked, which a future tunnel re-using the old name would have
    /// evict-killed under it at a random time.
    pub fn rename_entry(&self, old: &str, new: &str) {
        if old == new {
            return;
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(child) = inner.children.remove(old) {
            inner.children.insert(new.to_owned(), child);
        }
        if let Some(rt) = inner.rt.remove(old) {
            inner.rt.insert(new.to_owned(), rt);
        }
        if let Some(ev) = inner.events.remove(old) {
            inner.events.insert(new.to_owned(), ev);
        }
    }

    // ---- Event ring buffer ---------------------------------------------

    /// Record a status-transition event for `name`.
    ///
    /// Appends `{ts, msg}` to the tunnel's deque and trims the front so the
    /// buffer never exceeds [`EVENT_BUFFER_LIMIT`] entries (oldest dropped).
    pub fn record(&self, name: &str, ts: f64, msg: impl Into<String>) {
        let event = TunnelEvent { ts, msg: msg.into() };
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let deque = inner.events.entry(name.to_owned()).or_default();
        deque.push_back(event);
        while deque.len() > EVENT_BUFFER_LIMIT {
            deque.pop_front();
        }
    }

    /// Return the recorded events for `name` as a `Vec` (oldest → newest).
    ///
    /// Returns an empty `Vec` if no events have been recorded for this tunnel.
    pub fn events(&self, name: &str) -> Vec<TunnelEvent> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match inner.events.get(name) {
            Some(deque) => deque.iter().cloned().collect(),
            None => vec![],
        }
    }

    /// Kill every registered `ssh -L` child and drain the children map.
    ///
    /// Sends SIGKILL (via `child.kill()`) to every live child, then `wait()`s
    /// to reap the process.  Both steps are best-effort — errors are logged and
    /// swallowed so teardown continues for the remaining children.  Panic-safe
    /// (recovers a poisoned mutex).
    pub fn kill_all_children(&self) {
        // Drain the children map UNDER the lock, then kill()+wait() OFF the lock.
        // Holding `inner` across child.wait() would pin the runtime mutex (which
        // store_child/remove/record all need) for the duration of every reap —
        // a slow/stuck child would wedge all tunnel runtime ops. Snapshot-then-IO
        // mirrors HostManagers::teardown_all.
        let drained: Vec<(String, _)> = {
            let mut inner = match self.inner.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            inner.children.drain().collect()
        };
        let count = drained.len();
        for (name, mut child) in drained {
            info!("[tunnel:{name}] kill_all_children: sending SIGKILL");
            if let Err(e) = child.kill() {
                warn!("[tunnel:{name}] kill_all_children: kill() error: {e}");
            }
            if let Err(e) = child.wait() {
                warn!("[tunnel:{name}] kill_all_children: wait() error: {e}");
            }
        }
        info!("kill_all_children: killed {count} tunnel child(ren)");
    }

    // ---- Boot auto-start bookkeeping ----------------------------------

    /// Returns `(startup_ts, auto_started)`.
    pub fn boot_state(&self) -> (f64, bool) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (inner.startup_ts, inner.auto_started)
    }

    /// Mark the one-shot boot auto-start as completed.
    pub fn mark_auto_started(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).auto_started = true;
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
/// * `is_direct`           — true for direct-mode tunnels (no SLURM job,
///   so squeue checks must never fire for them).
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
    is_direct: bool,
) -> TunnelAction {
    use TunnelStatusKind::*;

    match status {
        // --- Stale + wants_alive: node confirmed gone from squeue ---
        // Stop the futile recover loop.  The tunnel stays Stale until the user
        // repicks a node (which restarts it).  Recovering would just churn
        // against a node that no longer exists.
        Stale if wants_alive => TunnelAction::Skip,

        // --- Tunnel wants to be alive but isn't (down) ---
        // Prioritise a due squeue check so we can detect a node that left
        // squeue even while the tunnel is down — otherwise a Failed tunnel
        // would recover forever and never notice its node ended.  Otherwise
        // fall back to a throttled recovery attempt.
        // Direct tunnels have no SLURM job — skip squeue entirely.
        Idle | Failed | PortBusy if wants_alive => {
            if !is_direct && now - last_squeue_ts >= SQUEUE_INTERVAL_SEC {
                TunnelAction::SqueueCheck
            } else if now - last_recovery_ts >= AUTO_RECOVERY_INTERVAL_SEC {
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

            // Case 3: squeue check due (compute tunnels only — direct have no job).
            if !is_direct && now - last_squeue_ts >= SQUEUE_INTERVAL_SEC {
                return TunnelAction::SqueueCheck;
            }

            TunnelAction::Skip
        }
    }
}

/// Whether a tunnel should be auto-started at boot.
///
/// Compute tunnels need a `last_node` (Python parity). Direct tunnels have no
/// node, so `is_direct` makes them eligible on the want flag alone.
pub fn should_autostart(
    auto_start: bool,
    wants_alive: bool,
    last_node: Option<&str>,
    is_direct: bool,
) -> bool {
    (auto_start || wants_alive) && (last_node.is_some() || is_direct)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_stop_after_failures_at_threshold() {
        // Below threshold → keep trying; at/above → auto-stop.
        assert!(!should_auto_stop_after_failures(0, RECOVERY_FAILURE_THRESHOLD));
        assert!(!should_auto_stop_after_failures(4, 5));
        assert!(should_auto_stop_after_failures(5, 5));
        assert!(should_auto_stop_after_failures(6, 5));
    }

    // Helper: a "now" timestamp far enough from 0 that throttles all pass.
    const NOW: f64 = 1_700_000_000.0;
    const OLD: f64 = 0.0; // treated as "long ago"

    // ---- tunnel_action: Case 0 — auto-recovery --------------------------

    #[test]
    fn wants_alive_idle_recovery_due_squeue_not_due_gives_recover() {
        let recent_squeue = NOW - 5.0; // squeue not due → recovery wins
        let action = tunnel_action(
            TunnelStatusKind::Idle,
            /*wants_alive=*/ true,
            None,
            /*port_bound=*/ false,
            None,
            /*last_recovery_ts=*/ OLD,
            /*last_squeue_ts=*/ recent_squeue,
            NOW,
            /*is_direct=*/ false,
        );
        assert_eq!(action, TunnelAction::Recover);
    }

    #[test]
    fn wants_alive_idle_squeue_due_gives_squeue_check() {
        // When squeue is due it takes priority over recovery for a down tunnel.
        let action = tunnel_action(
            TunnelStatusKind::Idle,
            /*wants_alive=*/ true,
            None,
            false,
            None,
            /*last_recovery_ts=*/ OLD,
            /*last_squeue_ts=*/ OLD, // due
            NOW,
            /*is_direct=*/ false,
        );
        assert_eq!(action, TunnelAction::SqueueCheck);
    }

    #[test]
    fn wants_alive_stale_gives_skip_not_recover() {
        // Regression guard: a Stale tunnel whose node is confirmed gone must
        // NOT keep recovering forever.  Even with both throttles elapsed it
        // must Skip (waiting for the user to repick a node).
        let action = tunnel_action(
            TunnelStatusKind::Stale,
            /*wants_alive=*/ true,
            None,
            false,
            None,
            /*last_recovery_ts=*/ OLD,
            /*last_squeue_ts=*/ OLD,
            NOW,
            /*is_direct=*/ false,
        );
        assert_eq!(action, TunnelAction::Skip);
    }

    #[test]
    fn wants_alive_port_busy_squeue_not_due_recovery_due_gives_recover() {
        let recent_squeue = NOW - 5.0; // squeue checked recently → not due
        let action = tunnel_action(
            TunnelStatusKind::PortBusy,
            true,
            None,
            false,
            None,
            /*last_recovery_ts=*/ OLD,
            /*last_squeue_ts=*/ recent_squeue,
            NOW,
            /*is_direct=*/ false,
        );
        assert_eq!(action, TunnelAction::Recover);
    }

    // ---- tunnel_action: DOWN tunnels now run squeue checks --------------

    #[test]
    fn wants_alive_failed_squeue_due_gives_squeue_check() {
        // The key fix: a down (Failed) tunnel that wants to be alive must run
        // a squeue check when one is due, so it can detect its node ended.
        let action = tunnel_action(
            TunnelStatusKind::Failed,
            /*wants_alive=*/ true,
            None,
            false,
            None,
            /*last_recovery_ts=*/ OLD,
            /*last_squeue_ts=*/ OLD, // squeue due
            NOW,
            /*is_direct=*/ false,
        );
        assert_eq!(action, TunnelAction::SqueueCheck);
    }

    #[test]
    fn wants_alive_failed_squeue_not_due_recovery_due_gives_recover() {
        let recent_squeue = NOW - 5.0; // not due (threshold 30 s)
        let action = tunnel_action(
            TunnelStatusKind::Failed,
            /*wants_alive=*/ true,
            None,
            false,
            None,
            /*last_recovery_ts=*/ OLD, // recovery due
            /*last_squeue_ts=*/ recent_squeue,
            NOW,
            /*is_direct=*/ false,
        );
        assert_eq!(action, TunnelAction::Recover);
    }

    #[test]
    fn wants_alive_failed_both_throttled_gives_skip() {
        let recent = NOW - 5.0; // both within their thresholds
        let action = tunnel_action(
            TunnelStatusKind::Failed,
            /*wants_alive=*/ true,
            None,
            false,
            None,
            /*last_recovery_ts=*/ recent,
            /*last_squeue_ts=*/ recent,
            NOW,
            /*is_direct=*/ false,
        );
        assert_eq!(action, TunnelAction::Skip);
    }

    #[test]
    fn wants_alive_idle_throttle_not_elapsed_gives_skip() {
        let recent = NOW - 5.0; // both throttles within their thresholds
        let action = tunnel_action(
            TunnelStatusKind::Idle,
            true,
            None,
            false,
            None,
            /*last_recovery_ts=*/ recent,
            /*last_squeue_ts=*/ recent,
            NOW,
            /*is_direct=*/ false,
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
            /*is_direct=*/ false,
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
            /*is_direct=*/ false,
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
            /*is_direct=*/ false,
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
            /*is_direct=*/ false,
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
            /*is_direct=*/ false,
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
            /*is_direct=*/ false,
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
            /*is_direct=*/ false,
        );
        assert_eq!(action, TunnelAction::Skip);
    }

    // ---- should_autostart -----------------------------------------------

    #[test]
    fn autostart_flag_with_node_gives_true() {
        assert!(should_autostart(true, false, Some("holygpu01"), false));
    }

    #[test]
    fn wants_alive_with_node_gives_true() {
        assert!(should_autostart(false, true, Some("holygpu01"), false));
    }

    #[test]
    fn autostart_flag_without_node_gives_false() {
        assert!(!should_autostart(true, false, None, false));
    }

    #[test]
    fn wants_alive_without_node_gives_false() {
        assert!(!should_autostart(false, true, None, false));
    }

    #[test]
    fn neither_flag_set_gives_false() {
        assert!(!should_autostart(false, false, Some("holygpu01"), false));
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
    fn store_child_evicts_stale_without_deadlock() {
        use std::process::Command;
        let rt = TunnelRuntime::new();

        // Store a first child.
        let first = Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn first sleep");
        rt.store_child("nb", first);

        // Storing a SECOND child for the same name must evict + reap the first
        // (two-phase, off-lock) and complete promptly — no deadlock against the
        // registry lock.
        let second = Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn second sleep");

        let start = std::time::Instant::now();
        rt.store_child("nb", second);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "store_child must not block / deadlock on eviction"
        );

        // The second child is now registered and alive; the first was reaped.
        assert_eq!(rt.child_alive("nb"), Some(true));

        // Clean up.
        rt.kill_child("nb");
        assert!(rt.take_child("nb").is_none());
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

    // ---- kill_all_children -----------------------------------------------

    #[test]
    fn kill_all_children_empty_does_not_panic() {
        let rt = TunnelRuntime::new();
        // No children registered — must be a no-op.
        rt.kill_all_children();
    }

    #[test]
    fn kill_all_children_kills_and_drains_registry() {
        use std::process::Command;
        let rt = TunnelRuntime::new();

        // Spawn a real long-lived child (mirrors existing child tests).
        let child = Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn sleep");

        rt.store_child("t1", child);

        // Confirm it is registered and alive.
        assert_eq!(rt.child_alive("t1"), Some(true));

        // kill_all_children should remove it and not panic.
        rt.kill_all_children();

        // After teardown the registry must be empty.
        assert!(rt.take_child("t1").is_none(), "child should be drained after kill_all_children");
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

    // ---- Event ring buffer -----------------------------------------------------

    #[test]
    fn event_record_and_events_round_trip_in_order() {
        let rt = TunnelRuntime::new();
        rt.record("t1", 1000.0, "connected");
        rt.record("t1", 1001.0, "probe ok");
        rt.record("t1", 1002.0, "alive");

        let evs = rt.events("t1");
        assert_eq!(evs.len(), 3);
        assert_eq!(evs[0].msg, "connected");
        assert_eq!(evs[0].ts, 1000.0);
        assert_eq!(evs[1].msg, "probe ok");
        assert_eq!(evs[2].msg, "alive");
        assert_eq!(evs[2].ts, 1002.0);
    }

    #[test]
    fn event_buffer_trims_to_limit_keeping_newest() {
        let rt = TunnelRuntime::new();
        for i in 0..250u32 {
            rt.record("t2", i as f64, format!("event-{i}"));
        }
        let evs = rt.events("t2");
        assert_eq!(evs.len(), EVENT_BUFFER_LIMIT, "buffer must be capped at EVENT_BUFFER_LIMIT");
        // The oldest 50 events (0..50) must have been dropped; newest 200 (50..250) retained.
        assert_eq!(evs[0].msg, "event-50", "oldest retained event should be event-50");
        assert_eq!(evs[199].msg, "event-249", "newest event should be event-249");
    }

    #[test]
    fn events_on_unknown_tunnel_returns_empty_vec() {
        let rt = TunnelRuntime::new();
        let evs = rt.events("ghost");
        assert!(evs.is_empty(), "events() for unknown tunnel must be empty");
    }

    #[test]
    fn remove_clears_event_buffer() {
        use std::process::Command;
        let rt = TunnelRuntime::new();

        // Record some events.
        rt.record("t3", 100.0, "started");
        rt.record("t3", 101.0, "alive");
        assert_eq!(rt.events("t3").len(), 2);

        // Spawn a child so remove() has something to kill.
        let child = Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn sleep");
        rt.store_child("t3", child);

        rt.remove("t3");

        assert!(rt.events("t3").is_empty(), "remove() must clear the event buffer");
    }

    // ---- note_stop_dead_flap (StopDead flap backoff) ---------------------

    #[test]
    fn one_off_drop_recovers_immediately() {
        // A single drop (even short-lived) keeps the responsive immediate-
        // recover behavior — backoff only after sustained flapping.
        let mut rt = TunnelRtState::default();
        let armed = note_stop_dead_flap(&mut rt, Some(2.0), 1000.0);
        assert!(!armed);
        assert_eq!(rt.consecutive_flaps, 1);
        assert_eq!(rt.last_recovery_attempt_ts, 0.0, "must recover immediately");
    }

    #[test]
    fn sustained_flapping_arms_backoff() {
        let mut rt = TunnelRtState::default();
        let mut now = 1000.0;
        for i in 1..TUNNEL_FLAP_THRESHOLD {
            assert!(!note_stop_dead_flap(&mut rt, Some(1.0), now), "no backoff at flap {i}");
            now += 5.0;
        }
        // The THRESHOLDth short-lived run arms the backoff.
        assert!(note_stop_dead_flap(&mut rt, Some(1.0), now), "backoff at threshold");
        assert_eq!(rt.consecutive_flaps, TUNNEL_FLAP_THRESHOLD);
        // The Recover gate (now - ts >= AUTO_RECOVERY_INTERVAL_SEC) must not
        // pass again until TUNNEL_FLAP_BACKOFF_SEC from now.
        let next_allowed = rt.last_recovery_attempt_ts + AUTO_RECOVERY_INTERVAL_SEC;
        assert!((next_allowed - (now + TUNNEL_FLAP_BACKOFF_SEC)).abs() < 1e-9);
    }

    #[test]
    fn long_stable_run_resets_flap_counter() {
        let mut rt = TunnelRtState { consecutive_flaps: TUNNEL_FLAP_THRESHOLD - 1, ..Default::default() };
        let armed = note_stop_dead_flap(&mut rt, Some(TUNNEL_FLAP_MIN_UPTIME_SEC + 1.0), 1000.0);
        assert!(!armed);
        assert_eq!(rt.consecutive_flaps, 0, "a stable run is not a flap");
        assert_eq!(rt.last_recovery_attempt_ts, 0.0);
    }

    #[test]
    fn unknown_uptime_leaves_counter_unchanged() {
        let mut rt = TunnelRtState { consecutive_flaps: 2, ..Default::default() };
        let armed = note_stop_dead_flap(&mut rt, None, 1000.0);
        assert!(!armed);
        assert_eq!(rt.consecutive_flaps, 2, "unknown uptime must not count either way");
    }

    // ---- direct-mode gating --------------------------------------------

    #[test]
    fn direct_down_wants_alive_squeue_due_gives_recover_not_squeue() {
        // A DIRECT tunnel has no SLURM job — even with squeue "due" it must
        // go straight to recovery, never SqueueCheck.
        let action = tunnel_action(
            TunnelStatusKind::Failed,
            /*wants_alive=*/ true,
            None,
            /*port_bound=*/ false,
            None,
            /*last_recovery_ts=*/ OLD,
            /*last_squeue_ts=*/ OLD, // would be "due" for a compute tunnel
            NOW,
            /*is_direct=*/ true,
        );
        assert_eq!(action, TunnelAction::Recover);
    }

    #[test]
    fn direct_alive_squeue_due_gives_skip_not_squeue() {
        let action = tunnel_action(
            TunnelStatusKind::Alive,
            true,
            Some(true),
            true,
            Some(true),
            OLD,
            /*last_squeue_ts=*/ OLD, // due
            NOW,
            /*is_direct=*/ true,
        );
        assert_eq!(action, TunnelAction::Skip);
    }

    #[test]
    fn direct_alive_child_dead_still_stop_dead() {
        // Direct tunnels still get the child-died health check.
        let action = tunnel_action(
            TunnelStatusKind::Alive,
            true,
            Some(false),
            true,
            Some(true),
            OLD,
            OLD,
            NOW,
            /*is_direct=*/ true,
        );
        assert_eq!(action, TunnelAction::StopDead);
    }

    #[test]
    fn direct_autostart_without_node_is_eligible() {
        // No last_node (direct tunnels never have one), but is_direct → eligible.
        assert!(should_autostart(false, true, None, /*is_direct=*/ true));
        assert!(should_autostart(true, false, None, /*is_direct=*/ true));
        // Not direct + no node → still NOT eligible (unchanged).
        assert!(!should_autostart(false, true, None, /*is_direct=*/ false));
    }
}
