//! Autonomous tunnel maintenance loop — Rust port of `TunnelManager.tick()`.
//!
//! Runs every ~1 s on its own thread (started from `server::run`).
//!
//! # Cases handled (mirrors Python `tick()`)
//!
//! * **Case 0 — auto-recovery**: if `wants_alive` and status ∈
//!   {Idle, Failed, Stale, PortBusy} and ≥15 s since last attempt → start.
//! * **Case 1 — child-died / ghost-alive**: if status == Alive but the child
//!   has exited OR the local port is no longer bound → stop(non-user) + recover.
//! * **Case 2 — disabled jump**: if status == Alive but the tunnel's active
//!   jump host is now `active == false` → stop (user stop, no recover).
//! * **Case 3 — squeue/stale**: every 30 s, discover nodes on the jump; count
//!   misses; at ≥2 misses re-check child identity under lock then mark stale.
//! * **Boot auto-start**: once after `startup_ts + 3 s`, start tunnels with
//!   `auto_start == true` OR (`wants_alive == true` AND `last_node.is_some()`).
//!
//! # Lock discipline
//!
//! The `State` mutex is **never** held while doing ssh I/O or blocking probes.
//! Pattern:
//!   1. Lock → snapshot fields → unlock.
//!   2. Do blocking work off-lock.
//!   3. Lock → write result back → unlock.
//!
//! The `TunnelRuntime` mutex is likewise held only for brief hash-map
//! operations; never across any I/O.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use log::{info, warn};

use a2fa_core::engine::State;
use a2fa_core::model::TunnelStatus;
use a2fa_core::tunnels::discovery::discover_nodes_via_control;
use a2fa_core::tunnels::probe::port_available;
use a2fa_core::tunnels::uptime::now_unix;
use a2fa_core::ssh::control::active_symlink_path;

use crate::tunnel_runtime::{
    note_stop_dead_flap, should_auto_stop_after_failures, should_autostart, tunnel_action,
    TunnelAction, TunnelRuntime, TunnelStatusKind, RECOVERY_FAILURE_THRESHOLD,
    STALE_MISS_THRESHOLD, TUNNEL_FLAP_BACKOFF_SEC, TUNNEL_FLAP_THRESHOLD,
};

/// How often the maintenance loop wakes up.
const MAINTENANCE_INTERVAL: Duration = Duration::from_millis(1000);

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Spawn the background tunnel-maintenance thread.
///
/// Returns immediately; the thread runs until the process exits.
pub fn start_tunnel_maintenance(
    state: Arc<Mutex<State>>,
    runtime: Arc<TunnelRuntime>,
    post_connect_running: Arc<Mutex<HashSet<String>>>,
) {
    // Degrade, never crash: see start_heartbeat. A panic here (the LAST of the
    // three boot spawns, after boot_autostart already fired) would crashloop the
    // daemon and re-trigger the spawn storm on every launchd respawn.
    if let Err(e) = std::thread::Builder::new()
        .name("tunnel-maintenance".into())
        .spawn(move || maintenance_loop(state, runtime, post_connect_running))
    {
        log::error!("failed to spawn tunnel-maintenance thread ({e}); tunnel auto-maintenance disabled this run");
    }
}

// ---------------------------------------------------------------------------
// Maintenance loop
// ---------------------------------------------------------------------------

fn maintenance_loop(
    state: Arc<Mutex<State>>,
    runtime: Arc<TunnelRuntime>,
    post_connect_running: Arc<Mutex<HashSet<String>>>,
) {
    loop {
        // Sleep is OUTSIDE the catch_unwind so the loop always paces itself,
        // even if a maintenance pass panics.
        std::thread::sleep(MAINTENANCE_INTERVAL);

        // Wrap the whole maintenance pass in catch_unwind so a panic in one
        // tunnel's processing is logged and the loop CONTINUES next interval,
        // instead of the maintenance thread dying and leaving all tunnels
        // unmanaged.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            maintenance_tick(&state, &runtime, &post_connect_running);
        }));
        if result.is_err() {
            warn!("tunnel-maintenance: a tick panicked — recovered, continuing next interval");
        }
    }
}

// ---------------------------------------------------------------------------
// One maintenance pass
// ---------------------------------------------------------------------------

/// Run one complete maintenance pass over all tunnels.
///
/// This is `pub(crate)` so `server.rs` can call it; it is also `#[cfg(test)]`-
/// visible so unit tests can invoke it directly.
pub fn maintenance_tick(
    state: &Arc<Mutex<State>>,
    runtime: &Arc<TunnelRuntime>,
    post_connect_running: &Arc<Mutex<HashSet<String>>>,
) {
    let now = now_unix();

    // ---- Boot auto-start ------------------------------------------------

    let (startup_ts, already_started) = runtime.boot_state();
    if !already_started && startup_ts > 0.0 && now - startup_ts >= crate::tunnel_runtime::BOOT_GRACE_SEC {
        run_boot_autostart(state, runtime, post_connect_running, now);
        runtime.mark_auto_started();
    }

    // ---- Per-tunnel maintenance ----------------------------------------

    // Snapshot tunnel list + relevant host state under a brief lock.
    let tunnel_snapshots: Vec<TunnelSnapshot> = {
        let guard = crate::lock_state(state);
        guard
            .tunnels
            .iter()
            .map(|t| TunnelSnapshot {
                name: t.name.clone(),
                local_port: t.local_port,
                remote_port: t.remote_port,
                status: t.status,
                wants_alive: t.wants_alive,
                active_jump: t.active_jump.clone(),
                last_node: t.last_node.clone(),
                last_user: t.last_user.clone(),
                post_connect_cmd: t.post_connect_cmd.clone(),
                jump_host_active: t.active_jump.as_deref().and_then(|jh| {
                    guard.hosts.iter().find(|h| h.host == jh).map(|h| h.active)
                }),
                jump_host_ready: t.active_jump.as_deref().and_then(|jh| {
                    guard.hosts.iter().find(|h| h.host == jh).map(|h| h.is_master_ready)
                }),
            })
            .collect()
    };

    for snap in tunnel_snapshots {
        process_tunnel(&snap, state, runtime, post_connect_running, now);
    }
}

// ---------------------------------------------------------------------------
// Per-tunnel processing
// ---------------------------------------------------------------------------

/// Lightweight snapshot of the per-tunnel fields we need for one maintenance pass.
#[derive(Clone)]
struct TunnelSnapshot {
    name: String,
    local_port: u16,
    #[allow(dead_code)]
    remote_port: u16,
    status: TunnelStatus,
    wants_alive: bool,
    active_jump: Option<String>,
    last_node: Option<String>,
    last_user: Option<String>,
    #[allow(dead_code)]
    post_connect_cmd: Option<String>,
    /// `Some(true/false)` → the tunnel's active_jump host's `active` flag.
    /// `None` → no active_jump recorded.
    jump_host_active: Option<bool>,
    /// `Some(true/false)` → the jump's `is_master_ready` flag.
    #[allow(dead_code)]
    jump_host_ready: Option<bool>,
}

fn process_tunnel(
    snap: &TunnelSnapshot,
    state: &Arc<Mutex<State>>,
    runtime: &Arc<TunnelRuntime>,
    post_connect_running: &Arc<Mutex<HashSet<String>>>,
    now: f64,
) {
    let name = &snap.name;

    // Read runtime counters (brief lock).
    let (last_recovery_ts, last_squeue_ts) = runtime
        .with_rt(name, |r| (r.last_recovery_attempt_ts, r.last_squeue_check_ts))
        .unwrap_or((0.0, 0.0));

    // Port check (off-lock, but cheap local bind).
    let port_bound = !port_available(snap.local_port);

    // Child alive check (brief registry lock, no I/O).
    let child_alive = runtime.child_alive(name);

    let status_kind = tunnel_status_kind(&snap.status);

    let action = tunnel_action(
        status_kind,
        snap.wants_alive,
        child_alive,
        port_bound,
        snap.jump_host_active,
        last_recovery_ts,
        last_squeue_ts,
        now,
        false, // is_direct — wired to snap.direct_host.is_some() in a later task
    );

    match action {
        TunnelAction::Skip => {}

        TunnelAction::Recover => {
            info!("[tunnel:{name}] auto-recover attempt (status={:?})", snap.status);
            // Update throttle timestamp first.
            runtime.with_rt_mut(name, |r| r.last_recovery_attempt_ts = now);
            // Kill any stale child handle before starting fresh.
            runtime.kill_child(name);
            do_tunnel_start(name, snap, state, runtime, post_connect_running);
        }

        TunnelAction::StopDead => {
            let reason = if child_alive == Some(false) {
                "child died"
            } else {
                "port not bound (ghost alive)"
            };
            warn!("[tunnel:{name}] {reason}, respawning");
            runtime.record(name, now, reason);
            // Peek the uptime of the run that just ended BEFORE accumulate_uptime
            // clears alive_since — it feeds the flap detector below.
            let uptime = runtime
                .with_rt(name, |r| r.alive_since)
                .flatten()
                .map(|since| (now - since).max(0.0));
            // Accumulate uptime before marking non-alive.
            accumulate_uptime(name, state, runtime, now);
            runtime.kill_child(name);
            mark_tunnel_idle(name, state, /*wants_alive_stays=*/ true, "respawning");
            // If wants_alive still set (non-user stop), attempt recovery. A
            // one-off drop recovers immediately, but a tunnel that keeps dying
            // right after each respawn (flapping) backs off — otherwise this
            // path kill+respawned it every ~1-2 s forever.
            let backed_off = runtime
                .with_rt_mut(name, |r| note_stop_dead_flap(r, uptime, now));
            if backed_off {
                warn!(
                    "[tunnel:{name}] flapping ({}+ short-lived runs) — backing off {}s",
                    TUNNEL_FLAP_THRESHOLD, TUNNEL_FLAP_BACKOFF_SEC
                );
                runtime.record(name, now, "flapping — backing off");
            }
        }

        TunnelAction::StopDisabledJump => {
            info!(
                "[tunnel:{name}] jump {:?} disabled, stopping",
                snap.active_jump
            );
            runtime.record(name, now, "jump disabled");
            accumulate_uptime(name, state, runtime, now);
            runtime.kill_child(name);
            mark_tunnel_idle(name, state, /*wants_alive_stays=*/ false, "jump disabled");
        }

        TunnelAction::SqueueCheck => {
            // Dispatch the squeue check to a short-lived worker thread so the
            // maintenance loop NEVER blocks on ssh — even though Layer 1 already
            // hard-bounds the ssh, a 15 s stall would still freeze every other
            // tunnel's health check for that whole window if run inline.
            //
            // Claim the throttle in the LOOP thread *before* spawning, so the
            // next tick (1 s later) sees last_squeue_check_ts == now and won't
            // re-dispatch until SQUEUE_INTERVAL_SEC has elapsed. This guarantees
            // at most one squeue worker per tunnel per interval.
            runtime.with_rt_mut(name, |r| r.last_squeue_check_ts = now);

            let name_owned = name.to_owned();
            let snap_clone = snap.clone();
            let state = Arc::clone(state);
            let runtime = Arc::clone(runtime);
            let spawn_res = std::thread::Builder::new()
                .name(format!("squeue-check:{name_owned}"))
                .spawn(move || {
                    run_squeue_check(&name_owned, &snap_clone, &state, &runtime, now);
                });
            if let Err(e) = spawn_res {
                warn!("[tunnel:{name}] failed to spawn squeue-check thread: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Boot auto-start
// ---------------------------------------------------------------------------

fn run_boot_autostart(
    state: &Arc<Mutex<State>>,
    runtime: &Arc<TunnelRuntime>,
    post_connect_running: &Arc<Mutex<HashSet<String>>>,
    now: f64,
) {
    info!("tunnel-maintenance: boot auto-start firing");

    // Snapshot candidate tunnels.
    let candidates: Vec<TunnelSnapshot> = {
        let guard = crate::lock_state(state);
        guard
            .tunnels
            .iter()
            .filter(|t| should_autostart(t.auto_start, t.wants_alive, t.last_node.as_deref(), false))
            .map(|t| TunnelSnapshot {
                name: t.name.clone(),
                local_port: t.local_port,
                remote_port: t.remote_port,
                status: t.status,
                wants_alive: t.wants_alive,
                active_jump: t.active_jump.clone(),
                last_node: t.last_node.clone(),
                last_user: t.last_user.clone(),
                post_connect_cmd: t.post_connect_cmd.clone(),
                jump_host_active: t.active_jump.as_deref().and_then(|jh| {
                    guard.hosts.iter().find(|h| h.host == jh).map(|h| h.active)
                }),
                jump_host_ready: t.active_jump.as_deref().and_then(|jh| {
                    guard.hosts.iter().find(|h| h.host == jh).map(|h| h.is_master_ready)
                }),
            })
            .collect()
    };

    for snap in &candidates {
        let name = &snap.name;
        info!("[tunnel:{name}] boot auto-start: starting");
        // An auto_start=true tunnel may have persisted wants_alive=false (user
        // stopped it last session). Boot auto-start means "make it alive", so
        // set wants_alive FIRST — do_tunnel_start (correctly) refuses to start
        // anything whose wants_alive is false, which used to silently veto the
        // auto_start flag at every boot. (Python: want = auto_start or
        // wants_alive → start.)
        {
            let mut guard = crate::lock_state(state);
            if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == *name) {
                t.wants_alive = true;
            }
        }
        runtime.with_rt_mut(name, |r| r.last_recovery_attempt_ts = now);
        do_tunnel_start(name, snap, state, runtime, post_connect_running);
    }
}

// ---------------------------------------------------------------------------
// Squeue / stale check (Case 3)
// ---------------------------------------------------------------------------

fn run_squeue_check(
    name: &str,
    snap: &TunnelSnapshot,
    state: &Arc<Mutex<State>>,
    runtime: &Arc<TunnelRuntime>,
    now: f64,
) {
    // Update the squeue check timestamp immediately so a slow discovery doesn't
    // trigger multiple concurrent checks.
    runtime.with_rt_mut(name, |r| r.last_squeue_check_ts = now);

    // A down tunnel has no active_jump, so pick a ready jump host the same way
    // recovery does (see `do_tunnel_start`): the first ready host matching the
    // tunnel's jump_candidates, or any ready host if candidates is None.  This
    // lets us run a squeue check even while the tunnel is down, so we can notice
    // its node has ended instead of recovering against it forever.
    let jump = match &snap.active_jump {
        Some(j) => j.clone(),
        None => {
            let guard = crate::lock_state(state);
            let candidates = guard
                .tunnels
                .iter()
                .find(|t| t.name == name)
                .and_then(|t| t.jump_candidates.clone());
            let picked = guard
                .hosts
                .iter()
                .find(|h| {
                    h.is_master_ready
                        && match &candidates {
                            Some(cs) => cs.contains(&h.host),
                            None => true,
                        }
                })
                .map(|h| h.host.clone());
            drop(guard);
            match picked {
                Some(j) => j,
                None => {
                    info!("[tunnel:{name}] squeue check: no ready jump, skipping");
                    return;
                }
            }
        }
    };

    let node = match &snap.last_node {
        Some(n) => n.clone(),
        None => return,
    };

    // Check that the jump master is ready (re-snapshot under brief lock).
    let master_ready = {
        let guard = crate::lock_state(state);
        guard
            .hosts
            .iter()
            .find(|h| h.host == jump)
            .map(|h| h.is_master_ready)
            .unwrap_or(false)
    };

    if !master_ready {
        info!("[tunnel:{name}] squeue check: jump {jump} not ready, skipping");
        return;
    }

    let cp = active_symlink_path(&jump);

    // Run squeue off-lock (blocking ssh command, ~100 ms). Pass the tunnel's
    // OWN cluster account (last_user): the jump may log in as a DIFFERENT
    // account (observed live: rkempner → rzhu while the job belongs to
    // shgao), and `-u $USER` through such a jump NEVER lists the job — every
    // check "missed" and a working tunnel was repeatedly marked stale.
    let jobs = match discover_nodes_via_control(&jump, &cp, snap.last_user.as_deref()) {
        Ok(j) => j,
        Err(e) => {
            warn!("[tunnel:{name}] squeue discovery error: {e}");
            // Update last_msg in State.
            let mut guard = crate::lock_state(state);
            if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == name) {
                t.last_msg = format!("squeue err: {}", &e.to_string()[..e.to_string().len().min(30)]);
            }
            return;
        }
    };

    let node_alive = jobs.iter().any(|j| j.node == node);

    if node_alive {
        runtime.with_rt_mut(name, |r| r.consecutive_squeue_misses = 0);
        return;
    }

    // Miss: increment counter.
    let misses = runtime
        .with_rt_mut(name, |r| {
            r.consecutive_squeue_misses += 1;
            r.consecutive_squeue_misses
        });

    info!(
        "[tunnel:{name}] squeue miss #{misses} (node={node})"
    );

    if misses >= STALE_MISS_THRESHOLD {
        // Re-check status under the lock before marking stale (mirrors Python's
        // per-tunnel lock re-check).  Mark stale when the tunnel is currently
        // Alive (the original case) OR when it is down but still wants to be
        // alive — i.e. it is stuck in the recover loop against a node that has
        // left squeue.  Marking it Stale stops that futile loop (the new
        // `tunnel_action` Stale arm returns Skip).
        let should_mark_stale = {
            let guard = crate::lock_state(state);
            guard
                .tunnels
                .iter()
                .find(|t| t.name == name)
                .map(|t| {
                    t.status == TunnelStatus::Alive
                        || (t.wants_alive
                            && matches!(
                                t.status,
                                TunnelStatus::Idle
                                    | TunnelStatus::Failed
                                    | TunnelStatus::PortBusy
                            ))
                })
                .unwrap_or(false)
        };

        if should_mark_stale {
            // Auto-stop: the compute node is gone, so the tunnel can never come
            // back to THIS node. Clear `wants_alive` so it stays stopped — both
            // now (Stale + !wants_alive → Skip) and across daemon restarts
            // (should_autostart is false). Previously `wants_alive` stayed true,
            // so every daemon restart re-ran the futile recover→stale burst.
            info!(
                "[tunnel:{name}] node {node} gone from squeue — auto-stopping tunnel"
            );
            runtime.record(name, now, format!("auto-stopped: node {node} ended"));
            accumulate_uptime(name, state, runtime, now);
            runtime.kill_child(name);

            {
                let mut guard = crate::lock_state(state);
                if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == name) {
                    t.status = TunnelStatus::Stale;
                    t.active_jump = None;
                    t.fail_count += 1;
                    t.wants_alive = false;
                    t.last_msg = format!("Auto-stopped: node {node} no longer in squeue");
                }
            }
            crate::handlers::tunnels::persist_tunnels(state);
            // Fresh slate if the user re-picks a node / re-enables later.
            runtime.with_rt_mut(name, |r| r.consecutive_recovery_failures = 0);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert `TunnelStatus` to the `TunnelStatusKind` enum used by the pure
/// decision function.
fn tunnel_status_kind(s: &TunnelStatus) -> TunnelStatusKind {
    match s {
        TunnelStatus::Idle => TunnelStatusKind::Idle,
        TunnelStatus::Failed => TunnelStatusKind::Failed,
        TunnelStatus::Stale => TunnelStatusKind::Stale,
        TunnelStatus::PortBusy => TunnelStatusKind::PortBusy,
        TunnelStatus::Starting => TunnelStatusKind::Starting,
        TunnelStatus::Alive => TunnelStatusKind::Alive,
    }
}

/// Fold `alive_since` into `total_uptime_sec` and clear the marker.
///
/// Mirrors Python's `_accumulate_uptime`.
fn accumulate_uptime(
    name: &str,
    state: &Arc<Mutex<State>>,
    runtime: &Arc<TunnelRuntime>,
    now: f64,
) {
    let alive_since = runtime.with_rt_mut(name, |r| {
        let s = r.alive_since;
        r.alive_since = None;
        s
    });

    if let Some(since) = alive_since {
        let delta = (now - since).max(0.0);
        let mut guard = crate::lock_state(state);
        if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == name) {
            t.total_uptime_sec += delta;
        }
    }
}

/// Mark a tunnel Idle under the State lock.
///
/// `wants_alive_stays=true` is used for non-user stops (child died etc.) so
/// the auto-recovery loop can pick it back up.
/// `wants_alive_stays=false` is used for user-initiated stops or
/// disabled-jump stops.
fn mark_tunnel_idle(
    name: &str,
    state: &Arc<Mutex<State>>,
    wants_alive_stays: bool,
    msg: &str,
) {
    let mut guard = crate::lock_state(state);
    if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == name) {
        t.status = TunnelStatus::Idle;
        t.active_jump = None;
        t.last_msg = msg.to_owned();
        if !wants_alive_stays {
            t.wants_alive = false;
        }
    }
}

/// Dispatch a tunnel start via `spawn_tunnel_start`.
///
/// Picks the first ready jump host that matches the tunnel's `jump_candidates`
/// (or any host if `jump_candidates` is None), then spawns the worker thread.
///
/// If no ready jump host is found, marks the tunnel Idle with an appropriate
/// message and returns.
/// Record one recovery failure for `name`. If consecutive failures cross
/// [`RECOVERY_FAILURE_THRESHOLD`], AUTO-STOP the tunnel: clear `wants_alive`
/// (so it stops retrying — on this run AND across daemon restarts), set a clear
/// "Auto-stopped" status, persist, and reset the counter so a later re-enable
/// gets a fresh set of attempts. Returns `true` iff it auto-stopped (the caller
/// surfaces it to the user).
fn note_recovery_failure_and_maybe_stop(
    name: &str,
    state: &Arc<Mutex<State>>,
    runtime: &Arc<TunnelRuntime>,
    now: f64,
) -> bool {
    let n = runtime.with_rt_mut(name, |r| {
        r.consecutive_recovery_failures += 1;
        r.consecutive_recovery_failures
    });
    if !should_auto_stop_after_failures(n, RECOVERY_FAILURE_THRESHOLD) {
        return false;
    }
    warn!("[tunnel:{name}] auto-stopped after {n} consecutive recovery failures");
    runtime.record(name, now, format!("auto-stopped: {n} consecutive failures"));
    {
        let mut guard = crate::lock_state(state);
        if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == name) {
            t.wants_alive = false;
            t.active_jump = None;
            t.last_msg = format!("Auto-stopped: {n} consecutive failures");
        }
    }
    crate::handlers::tunnels::persist_tunnels(state);
    // Fresh slate if the user re-enables it later.
    runtime.with_rt_mut(name, |r| r.consecutive_recovery_failures = 0);
    true
}

fn do_tunnel_start(
    name: &str,
    snap: &TunnelSnapshot,
    state: &Arc<Mutex<State>>,
    runtime: &Arc<TunnelRuntime>,
    post_connect_running: &Arc<Mutex<HashSet<String>>>,
) {
    // Re-snapshot under lock to get fresh jump candidates / readiness.
    let start_info: Option<(String, String, String, u16, u16, Option<String>)> = {
        let mut guard = crate::lock_state(state);

        // Guard: re-check wants_alive under lock (concurrent user-stop may have
        // cleared it while we were deciding to recover off-lock — mirrors Python's
        // `_auto_recovery` flag re-check).
        let t = match guard.tunnels.iter().find(|t| t.name == name) {
            Some(t) => t,
            None => return, // tunnel was removed
        };
        if !t.wants_alive {
            info!("[tunnel:{name}] do_tunnel_start: wants_alive cleared by user — skipping");
            return;
        }
        if matches!(t.status, TunnelStatus::Alive | TunnelStatus::Starting) {
            return; // already in flight
        }

        // Pick a ready jump host.
        let jump = {
            let candidates = t.jump_candidates.clone();
            guard.hosts.iter().find(|h| {
                h.is_master_ready && match &candidates {
                    Some(cs) => cs.contains(&h.host),
                    None => true,
                }
            }).map(|h| h.host.clone())
        };

        let t = guard.tunnels.iter_mut().find(|t| t.name == name).unwrap();

        let node = match t.last_node.clone() {
            Some(n) => n,
            None => {
                t.status = TunnelStatus::Idle;
                t.last_msg = "no node — press Enter to pick".into();
                return;
            }
        };

        let jump = match jump {
            Some(j) => j,
            None => {
                t.status = TunnelStatus::Idle;
                t.last_msg = "waiting for jump host".into();
                return;
            }
        };

        let user = t
            .last_user
            .clone()
            .unwrap_or_else(|| std::env::var("USER").unwrap_or_default());

        if user.is_empty() {
            t.status = TunnelStatus::Failed;
            t.last_msg = "no user (set last_user in tunnels.json)".into();
            return;
        }

        let local_port = t.local_port;
        let remote_port = t.remote_port;
        let post_cmd = t.post_connect_cmd.clone();

        t.status = TunnelStatus::Starting;
        t.active_jump = Some(jump.clone());
        t.last_msg = format!("starting via {jump}");

        Some((jump, user, node, local_port, remote_port, post_cmd))
    };

    if let Some((jump, user, node, local_port, remote_port, post_cmd)) = start_info {
        // Set alive_since when the tunnel eventually becomes Alive.
        // The worker thread writes Alive into State; we hook the alive_since
        // update through a wrapper approach: the runtime state gets updated
        // when the tunnel transitions to Alive (see `spawn_tunnel_start_with_runtime`).
        spawn_tunnel_start_with_runtime(
            name.to_owned(),
            jump,
            user,
            node,
            local_port,
            remote_port,
            snap.local_port,
            post_cmd,
            Arc::clone(state),
            Arc::clone(post_connect_running),
            Arc::clone(runtime),
        );
    }
}

// ---------------------------------------------------------------------------
// spawn_tunnel_start_with_runtime
// ---------------------------------------------------------------------------

/// Like `workers::spawn_tunnel_start` but also:
/// * Stores the `Child` in the `TunnelRuntime` registry on success.
/// * Sets `alive_since` in the runtime when the tunnel reaches Alive.
///
/// This is the maintenance loop's entry point for starting a tunnel.
#[allow(clippy::too_many_arguments)]
fn spawn_tunnel_start_with_runtime(
    name: String,
    jump: String,
    user: String,
    node: String,
    local_port: u16,
    remote_port: u16,
    _snap_local_port: u16,
    post_connect_cmd: Option<String>,
    state: Arc<Mutex<State>>,
    post_connect_running: Arc<Mutex<HashSet<String>>>,
    runtime: Arc<TunnelRuntime>,
) {
    let spawn_name = name.clone();
    let spawn_state = Arc::clone(&state);
    let spawn_runtime = Arc::clone(&runtime);
    let spawn_res = std::thread::Builder::new()
        .name(format!("maintenance-start:{name}"))
        .spawn(move || {
            use a2fa_core::tunnels::forward::{probe_and_settle, start_forward, ProbeOutcome};
            use a2fa_core::tunnels::post_connect::run_post_connect;

            info!("[tunnel:{name}] maintenance: starting via {jump}");

            let child = match start_forward(&jump, &user, &node, local_port, remote_port) {
                Ok(c) => c,
                Err(e) => {
                    warn!("[tunnel:{name}] maintenance: spawn failed: {e}");
                    let msg = format!("spawn failed: {e}");
                    runtime.record(&name, now_unix(), &msg);
                    let mut guard = crate::lock_state(&state);
                    if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == name) {
                        t.fail_count += 1;
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
                    let now = now_unix();

                    // Abort check: the user may have hit Stop during the start
                    // (while `Starting`, the child isn't registered yet so
                    // tunnel_stop had nothing to kill). Honor the abort instead
                    // of resurrecting + persisting wants_alive=true over it.
                    let aborted = {
                        let guard = crate::lock_state(&state);
                        guard
                            .tunnels
                            .iter()
                            .find(|t| t.name == name)
                            .map(|t| !t.wants_alive)
                            .unwrap_or(true) // deleted mid-start → abort
                    };
                    if aborted {
                        info!("[tunnel:{name}] stopped during start — killing fresh forward (abort honored)");
                                                a2fa_core::tunnels::forward::stop_forward(child);
                        runtime.record(&name, now, "start aborted by user stop");
                        return;
                    }

                    // Store child in registry.
                    runtime.store_child(&name, child);

                    // Set alive_since in the runtime.
                    runtime.with_rt_mut(&name, |r| r.alive_since = Some(now));

                    // Record connect event.
                    runtime.record(&name, now, format!("connected via {jump} → {node}:{remote_port}"));

                    // Update State.
                    {
                        let mut guard = crate::lock_state(&state);
                        if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == name) {
                            t.status = TunnelStatus::Alive;
                            t.wants_alive = true;
                            t.last_alive_at = now;
                            t.connect_count += 1;
                            t.active_jump = Some(jump.clone());
                            t.last_msg = format!("via {jump}");
                        }
                    }
                    // A successful connect clears the consecutive-failure tally.
                    runtime.with_rt_mut(&name, |r| r.consecutive_recovery_failures = 0);

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
                            node.clone(),
                            jump.clone(),
                            post_connect_running,
                        );
                    }
                }
                Ok((outcome @ (ProbeOutcome::TimedOut | ProbeOutcome::ChildExited), mut child)) => {
                    let msg = match outcome {
                        ProbeOutcome::ChildExited => {
                            // Reap the exited child (TimedOut already kills+waits
                            // inside probe_and_settle).
                            let _ = child.kill();
                            let _ = child.wait();
                            format!("local port {local_port} in use by another process (ssh exited at bind)")
                        }
                        _ => "probe timed out".to_string(),
                    };
                    warn!("[tunnel:{name}] maintenance: {msg}");
                    runtime.record(&name, now_unix(), &msg);
                    {
                        let mut guard = crate::lock_state(&state);
                        if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == name) {
                            t.fail_count += 1;
                            t.status = TunnelStatus::Failed;
                            t.last_msg = msg;
                            t.active_jump = None;
                        }
                    }
                    note_recovery_failure_and_maybe_stop(&name, &state, &runtime, now_unix());
                }
                Err(e) => {
                    warn!("[tunnel:{name}] maintenance: probe error: {e}");
                    let msg = format!("probe error: {e}");
                    runtime.record(&name, now_unix(), &msg);
                    {
                        let mut guard = crate::lock_state(&state);
                        if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == name) {
                            t.fail_count += 1;
                            t.status = TunnelStatus::Failed;
                            t.last_msg = msg;
                            t.active_jump = None;
                        }
                    }
                    note_recovery_failure_and_maybe_stop(&name, &state, &runtime, now_unix());
                }
            }
        });
    if let Err(e) = spawn_res {
        // Transient EAGAIN: the worker never ran, so the tunnel is stuck at the
        // Starting status that `do_tunnel_start` set. Reset it to Failed under
        // the lock so the next maintenance tick's Recover arm retries it.
        warn!("[tunnel:{spawn_name}] maintenance: failed to spawn start thread: {e}");
        spawn_runtime.record(&spawn_name, now_unix(), format!("spawn failed: {e}"));
        let mut guard = crate::lock_state(&spawn_state);
        if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == spawn_name) {
            t.fail_count += 1;
            t.status = TunnelStatus::Failed;
            t.last_msg = format!("spawn failed: {e}");
            t.active_jump = None;
        }
    }
}
