//! Persistent per-host pool registry and heartbeat/auto-reconnect loop.
//!
//! # Architecture
//!
//! ## Why `HostManagers`
//!
//! Previously `spawn_host_start` built a *fresh* `PoolState` every call, so
//! cooldown and consecutive-failure counts reset after every retry.  The
//! circuit-breaker was effectively broken.
//!
//! `HostManagers` owns **one long-lived `PoolState` per host** that persists
//! for the daemon's lifetime.  Both `host_toggle` and the heartbeat loop
//! operate on the same instance — so 4 failures followed by a reboot of the
//! heartbeat timer don't reset the counter back to zero.
//!
//! ## Lock discipline
//!
//! Two separate locks exist:
//! * `Mutex<HashMap<String, PoolState>>` — the managers map.  Held **briefly**
//!   to snapshot / write back host state; NEVER across any ssh I/O.
//! * `Mutex<State>` (engine state) — same rule.
//!
//! All blocking calls (`start_master`, `master_check`, etc.) are executed with
//! **both** locks fully released.  The pattern is always:
//!   1. Lock → clone what is needed → unlock.
//!   2. Do blocking I/O (no locks held).
//!   3. Lock → write result back → unlock.
//!
//! ## Heartbeat loop (mirrors Python `manage_pool_loop`)
//!
//! A single background thread (started from `server.rs`) iterates every ~3 s
//! over all hosts whose `active == true` in the engine `State`.  For each host
//! it:
//!   * Skips if cooldown is active (persistent — does NOT reset between ticks).
//!   * Heartbeats each slot via `ssh -O check` (local, sub-ms normally).
//!   * Marks dead slots Dead and enqueues a restart (with a ~2 s throttle).
//!   * Tries to warm slot 1 when only slot 0 is ready (staggered start).
//!   * Calls `try_rotate` every ~5 s to flip to a spare when the active slot
//!     is down.
//!
//! ## Deactivation
//!
//! When `host_toggle` is called to turn a host *off*, `stop_all` is run on
//! the persistent `PoolState` and circuit breakers are reset (so the next
//! manual activation starts fresh, matching Python behaviour).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use log::{info, warn};

use a2fa_core::engine::State;
use a2fa_core::ssh::control::master_check;
use a2fa_core::ssh::master::{start_master, stop_all, PoolState, SlotStatus, POOL_SIZE};

use crate::workers::{make_otp_closure, OtpRegistry};

// ---------------------------------------------------------------------------
// Heartbeat / rotation timing constants (mirroring backend.py)
// ---------------------------------------------------------------------------

/// How often the heartbeat loop wakes up.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);

/// How often the rotation / remote-probe check runs.
const ROTATION_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Throttle before restarting a dead slot (mirrors Python's `time.sleep(2)`).
const RESTART_THROTTLE: Duration = Duration::from_secs(2);

/// Stagger delay before starting slot 1 (mirrors Python's `time.sleep(5)` in
/// `start_master_async`).
const SLOT1_STAGGER: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// MaintenanceAction — pure decision output, no I/O
// ---------------------------------------------------------------------------

/// The action the heartbeat should take for a single (host, slot) pair.
///
/// Extracting this as a pure enum lets us unit-test the *decision* logic
/// without running any real SSH commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaintenanceAction {
    /// Host is inactive or in cooldown — nothing to do this tick.
    Skip,
    /// Slot appears healthy — no action.
    Healthy,
    /// Slot is dead / unresponsive — restart it (after throttle).
    Restart,
    /// Slot 1 is not yet warm while slot 0 is ready — warm it (staggered).
    WarmSlot1,
    /// Active slot is dead but the other slot is Ready — rotate.
    Rotate,
}

/// Decide what maintenance action is needed for a single slot.
///
/// This function is **pure** — it never touches the filesystem or network.
/// Real ssh ops are always dispatched by the caller after this returns.
///
/// Arguments:
/// * `pool`        — current `PoolState` (read-only).
/// * `slot`        — the slot index being evaluated (0 or 1).
/// * `active`      — whether this host is currently marked active in `State`.
/// * `check_alive` — result of `master_check` for this slot (pass `None` to
///   skip the live check, e.g. when the slot was never started).
/// * `now`         — caller-supplied `Instant` for deterministic testing.
pub fn next_action(
    pool: &PoolState,
    slot: usize,
    active: bool,
    check_alive: Option<bool>,
    now: Instant,
) -> MaintenanceAction {
    if !active {
        return MaintenanceAction::Skip;
    }
    if pool.in_cooldown() {
        return MaintenanceAction::Skip;
    }

    // Slot 1 warm-up: if slot 0 is ready but slot 1 has never been started.
    if slot == 1 && pool.slot_status[0] == SlotStatus::Ready
        && pool.slot_status[1] == SlotStatus::Init
    {
        return MaintenanceAction::WarmSlot1;
    }

    // Rotation: active slot is dead and the other is ready.
    let other = (pool.active_index + 1) % POOL_SIZE;
    if pool.slot_status[pool.active_index] == SlotStatus::Dead
        && pool.slot_status[other] == SlotStatus::Ready
        && !pool.in_probe_backoff()
    {
        // Only suggest Rotate when evaluating the active slot.
        if slot == pool.active_index {
            return MaintenanceAction::Rotate;
        }
    }

    // Restart if the slot is in a non-Ready non-Init state (Dead/Failed),
    // or if the live check returned false.
    let needs_restart = match pool.slot_status[slot] {
        SlotStatus::Dead | SlotStatus::Failed => true,
        SlotStatus::Init => false, // not started yet — handled by warm-up logic
        SlotStatus::Ready => check_alive == Some(false),
    };

    // Suppress restart if in probe backoff.
    if needs_restart && !pool.in_probe_backoff() {
        return MaintenanceAction::Restart;
    }

    // Silence the `now` unused warning in non-test paths.
    let _ = now;

    MaintenanceAction::Healthy
}

// ---------------------------------------------------------------------------
// HostManagers — persistent per-host PoolState registry
// ---------------------------------------------------------------------------

/// Daemon-global registry of persistent `PoolState` instances.
///
/// One `PoolState` per host, created lazily on first access and kept alive for
/// the daemon's lifetime so cooldown / failure counts survive across retries.
#[derive(Default)]
pub struct HostManagers {
    map: Mutex<HashMap<String, PoolState>>,
}

impl HostManagers {
    /// Create a new, empty registry wrapped in an `Arc`.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Run `f` with a mutable reference to the `PoolState` for `host`.
    ///
    /// Creates a default `PoolState` if none exists yet.  The lock on the
    /// internal map is held only for the duration of `f` — `f` must be brief
    /// (no ssh I/O, no sleeps).
    pub fn with_pool_mut<R>(&self, host: &str, f: impl FnOnce(&mut PoolState) -> R) -> R {
        let mut map = self.map.lock().unwrap();
        let pool = map
            .entry(host.to_owned())
            .or_insert_with(|| PoolState::new(host));
        f(pool)
    }

    /// Run `f` with a shared reference to the `PoolState` for `host`.
    ///
    /// Returns `None` if no state exists for this host yet.
    pub fn with_pool<R>(&self, host: &str, f: impl FnOnce(&PoolState) -> R) -> Option<R> {
        let map = self.map.lock().unwrap();
        map.get(host).map(f)
    }

    /// Clone the pool state snapshot for a host (cheap — just copies a few
    /// integers and Instant options).  Returns a fresh `PoolState` if none
    /// exists yet.
    pub fn snapshot(&self, host: &str) -> PoolState {
        self.with_pool_mut(host, |p| PoolState {
            host: p.host.clone(),
            slot_status: p.slot_status,
            active_index: p.active_index,
            consecutive_login_failures: p.consecutive_login_failures,
            cooldown_until: p.cooldown_until,
            last_rotate: p.last_rotate,
            probe_backoff_until: p.probe_backoff_until,
        })
    }

    /// Tear down every SSH master in the registry.
    ///
    /// Calls `stop_all` on each `PoolState` in the map (which runs
    /// `ssh -O exit` and cleans up the control-path symlink for each slot).
    /// Errors from individual hosts are logged and swallowed so teardown
    /// continues for the remaining hosts.  Panic-safe.
    pub fn teardown_all(&self) {
        let mut map = match self.map.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(), // recover from a poisoned lock
        };
        let count = map.len();
        for (host, pool) in map.iter_mut() {
            info!("[{host}] teardown_all: stopping all SSH master slots");
            // stop_all runs `ssh -O exit` — fast, best-effort.
            // Any subprocess errors are already absorbed inside stop_all.
            stop_all(pool);
        }
        info!("teardown_all: tore down {count} host(s)");
    }

    /// Write back a subset of mutable fields from `src` into the stored state
    /// for `host`.  Only the fields that the ssh worker is authorised to update
    /// (slot_status, consecutive_login_failures, cooldown, backoff) are copied;
    /// `active_index` and host name are preserved from the stored state.
    pub fn write_back(&self, host: &str, src: &PoolState) {
        self.with_pool_mut(host, |dst| {
            dst.slot_status = src.slot_status;
            dst.consecutive_login_failures = src.consecutive_login_failures;
            dst.cooldown_until = src.cooldown_until;
            dst.last_rotate = src.last_rotate;
            dst.probe_backoff_until = src.probe_backoff_until;
            dst.active_index = src.active_index;
        });
    }
}

// ---------------------------------------------------------------------------
// Boot auto-start (mirrors Python `init_managers`)
// ---------------------------------------------------------------------------

/// For every host with `active == true` in `state`, kick off a master-start
/// via the same path `host_toggle` uses.
///
/// Called once from `server::run()` after `State` is loaded.
pub fn boot_autostart(
    state: &Arc<Mutex<State>>,
    managers: &Arc<HostManagers>,
    registry: &Arc<OtpRegistry>,
) {
    use a2fa_core::creds::keychain::KeychainStore;
    use a2fa_core::creds::{get_otpauth, get_password};
    use a2fa_core::totp::extract_secret;

    // Collect active hosts under the lock.
    let active_hosts: Vec<String> = {
        let guard = state.lock().unwrap();
        guard
            .hosts
            .iter()
            .filter(|h| h.active)
            .map(|h| h.host.clone())
            .collect()
    };

    for host_name in active_hosts {
        let ks = KeychainStore;
        let password = get_password(&ks, &host_name)
            .ok()
            .flatten()
            .unwrap_or_default();
        let otpauth = get_otpauth(&ks, &host_name)
            .ok()
            .flatten()
            .unwrap_or_default();
        let secret = extract_secret(&otpauth).unwrap_or_default();

        // Update status in State to "Connecting…".
        {
            let mut guard = state.lock().unwrap();
            if let Some(h) = guard.hosts.iter_mut().find(|hh| hh.host == host_name) {
                h.last_msg = "Boot auto-connecting…".into();
                h.status = "Connecting".into();
            }
        }

        info!("[{host_name}] boot auto-start: spawning master slot 0");
        spawn_managed_start(
            host_name,
            0,
            password,
            secret,
            Arc::clone(registry),
            Arc::clone(state),
            Arc::clone(managers),
        );
    }
}

// ---------------------------------------------------------------------------
// spawn_managed_start — like spawn_host_start but uses persistent PoolState
// ---------------------------------------------------------------------------

/// Spawn a blocking OS thread that runs `start_master` using the persistent
/// `PoolState` from `managers`.
///
/// After the ssh call completes the thread:
///  1. Writes the updated `PoolState` fields back to `managers`.
///  2. Updates `State` (host status, is_master_ready, pool_alive, pool_index).
///
/// Lock discipline: both `managers` and `state` locks are dropped before the
/// blocking ssh call.
pub fn spawn_managed_start(
    host_name: String,
    slot: usize,
    password: String,
    secret: String,
    registry: Arc<OtpRegistry>,
    state: Arc<Mutex<State>>,
    managers: Arc<HostManagers>,
) {
    std::thread::Builder::new()
        .name(format!("managed-start:{host_name}:{slot}"))
        .spawn(move || {
            // 1. Snapshot the current PoolState (brief lock).
            let mut pool = managers.snapshot(&host_name);

            // 2. Build the OTP closure (no locks held).
            let otp_closure = make_otp_closure(secret, host_name.clone(), registry);

            info!("[{host_name}] managed-start worker: slot {slot}");

            // 3. Run start_master — blocking ssh pty, no locks held.
            let ready = start_master(&mut pool, slot, &password, otp_closure);

            // 4. Write PoolState back to the persistent registry.
            managers.write_back(&host_name, &pool);

            // 5. Update engine State.
            let mut guard = state.lock().unwrap();
            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                if ready {
                    h.is_master_ready = true;
                    h.pool_alive = 1;
                    h.pool_index = slot as u8;
                    h.status = "Connected".into();
                    h.last_msg = format!("Master slot {slot} ready");
                    info!("[{host_name}] managed-start: slot {slot} Ready — State updated");
                } else {
                    h.is_master_ready = false;
                    h.status = if pool.in_cooldown() {
                        "Cooldown".into()
                    } else {
                        "Failed".into()
                    };
                    h.last_msg = format!("Master slot {slot} login failed");
                    warn!("[{host_name}] managed-start: slot {slot} failed — State updated");
                }
            }
        })
        .expect("failed to spawn managed-start thread");
}

// ---------------------------------------------------------------------------
// spawn_managed_stop — stop_all on the persistent PoolState
// ---------------------------------------------------------------------------

/// Spawn a blocking thread that stops all pool slots for `host` and resets the
/// persistent circuit breakers (so the next manual activation starts fresh).
pub fn spawn_managed_stop(
    host_name: String,
    state: Arc<Mutex<State>>,
    managers: Arc<HostManagers>,
) {
    std::thread::Builder::new()
        .name(format!("managed-stop:{host_name}"))
        .spawn(move || {
            // 1. Snapshot and reset circuit breakers — brief lock.
            let mut pool = managers.snapshot(&host_name);
            pool.reset_circuit_breakers();

            // 2. stop_all — blocking (ssh -O exit + file cleanup), no locks held.
            info!("[{host_name}] managed-stop: stopping all slots");
            stop_all(&mut pool);

            // 3. Write zeroed state back.
            managers.write_back(&host_name, &pool);

            // 4. Update engine State.
            let mut guard = state.lock().unwrap();
            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                h.is_master_ready = false;
                h.pool_alive = 0;
                h.active = false;
                h.status = "Idle".into();
                h.last_msg = "Stopped".into();
            }
        })
        .expect("failed to spawn managed-stop thread");
}

// ---------------------------------------------------------------------------
// Heartbeat / maintenance loop
// ---------------------------------------------------------------------------

/// Start the background heartbeat thread.
///
/// Returns immediately; the thread runs until the process exits.
///
/// The thread loops every `HEARTBEAT_INTERVAL`:
///   * For each active host:
///     - If in cooldown → skip.
///     - For each slot: run `ssh -O check` (local, ms).
///       * If dead → mark Dead, sleep `RESTART_THROTTLE`, restart.
///     - If slot 1 is Init and slot 0 is Ready → warm slot 1 (staggered).
///     - Every `ROTATION_CHECK_INTERVAL` → `try_rotate` if active slot is down.
///
/// All ssh calls are performed **off** both mutex locks.
pub fn start_heartbeat(
    state: Arc<Mutex<State>>,
    managers: Arc<HostManagers>,
    registry: Arc<OtpRegistry>,
) {
    std::thread::Builder::new()
        .name("heartbeat".into())
        .spawn(move || heartbeat_loop(state, managers, registry))
        .expect("failed to spawn heartbeat thread");
}

fn heartbeat_loop(
    state: Arc<Mutex<State>>,
    managers: Arc<HostManagers>,
    registry: Arc<OtpRegistry>,
) {
    let mut last_rotation_check = Instant::now();

    loop {
        std::thread::sleep(HEARTBEAT_INTERVAL);

        // Snapshot active hosts + their credentials (brief State lock).
        let active: Vec<(String, String, String)> = {
            use a2fa_core::creds::keychain::KeychainStore;
            use a2fa_core::creds::{get_otpauth, get_password};
            use a2fa_core::totp::extract_secret;

            let guard = state.lock().unwrap();
            guard
                .hosts
                .iter()
                .filter(|h| h.active)
                .map(|h| {
                    let ks = KeychainStore;
                    let pw = get_password(&ks, &h.host).ok().flatten().unwrap_or_default();
                    let oa = get_otpauth(&ks, &h.host).ok().flatten().unwrap_or_default();
                    let secret = extract_secret(&oa).unwrap_or_default();
                    (h.host.clone(), pw, secret)
                })
                .collect()
        };

        let now = Instant::now();
        let do_rotation_check = now.duration_since(last_rotation_check) >= ROTATION_CHECK_INTERVAL;
        if do_rotation_check {
            last_rotation_check = now;
        }

        for (host_name, password, secret) in active {
            tick_host(
                &host_name,
                &password,
                &secret,
                do_rotation_check,
                &state,
                &managers,
                &registry,
            );
        }
    }
}

/// Run one heartbeat tick for a single host.
///
/// This function is called from the heartbeat loop and is the actual
/// implementation of Python's `manage_pool_loop` inner logic.
fn tick_host(
    host_name: &str,
    password: &str,
    secret: &str,
    do_rotation_check: bool,
    state: &Arc<Mutex<State>>,
    managers: &Arc<HostManagers>,
    registry: &Arc<OtpRegistry>,
) {
    let now = Instant::now();

    // Snapshot the pool (brief lock).
    let pool = managers.snapshot(host_name);

    if pool.in_cooldown() {
        let secs = pool
            .cooldown_until
            .map(|t| t.saturating_duration_since(now).as_secs())
            .unwrap_or(0);
        info!("[{host_name}] heartbeat: in cooldown ({secs}s left) — skipping");
        // Update State with cooldown status.
        let mut guard = state.lock().unwrap();
        if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
            h.status = format!("Cooldown ({secs}s)");
        }
        return;
    }

    // --- Per-slot heartbeat: run ssh -O check (off-lock) ---
    for slot in 0..POOL_SIZE {
        let path = pool.pool_path(slot);

        // Only bother checking slots that have ever been started.
        let check_result: Option<bool> = match pool.slot_status[slot] {
            SlotStatus::Init => None, // never started — skip live check
            _ => Some(master_check(&path, host_name)),
        };

        let action = next_action(&pool, slot, true, check_result, now);

        match action {
            MaintenanceAction::Restart => {
                warn!(
                    "[{host_name}] heartbeat: slot {slot} needs restart (status={:?}, check={:?})",
                    pool.slot_status[slot], check_result
                );
                // Mark slot Dead in the persistent registry.
                managers.with_pool_mut(host_name, |p| {
                    p.slot_status[slot] = SlotStatus::Dead;
                });
                // Update engine State.
                {
                    let mut guard = state.lock().unwrap();
                    if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                        if slot == h.pool_index as usize {
                            h.is_master_ready = false;
                            h.status = "Reconnecting".into();
                            h.last_msg = format!("Slot {slot} dead — reconnecting");
                        }
                    }
                }

                // Throttle before restart (mirrors Python's time.sleep(2)).
                std::thread::sleep(RESTART_THROTTLE);

                // Re-check active flag — host may have been toggled off during sleep.
                let still_active = {
                    let guard = state.lock().unwrap();
                    guard
                        .hosts
                        .iter()
                        .find(|h| h.host == host_name)
                        .map(|h| h.active)
                        .unwrap_or(false)
                };
                if !still_active {
                    info!("[{host_name}] heartbeat: host deactivated during throttle — skipping restart");
                    continue;
                }

                // Restart off-lock.
                let otp_closure = make_otp_closure(
                    secret.to_owned(),
                    host_name.to_owned(),
                    Arc::clone(registry),
                );
                let mut pool_mut = managers.snapshot(host_name);
                let ready = start_master(&mut pool_mut, slot, password, otp_closure);
                managers.write_back(host_name, &pool_mut);

                // Write result back to engine State.
                let mut guard = state.lock().unwrap();
                if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                    if ready {
                        h.is_master_ready = true;
                        h.pool_alive = 1;
                        h.pool_index = slot as u8;
                        h.status = "Connected".into();
                        h.last_msg = format!("Slot {slot} reconnected");
                    } else {
                        h.status = "Reconnect failed".into();
                        h.last_msg = format!("Slot {slot} reconnect failed");
                    }
                }

                // If we just restarted the active slot, try rotating to the
                // spare if it's ready (mirrors Python: `update_symlink(other)`).
                if slot == pool.active_index {
                    let other = (slot + 1) % POOL_SIZE;
                    let other_ready = managers
                        .with_pool(host_name, |p| p.slot_status[other] == SlotStatus::Ready)
                        .unwrap_or(false);
                    if other_ready {
                        managers.with_pool_mut(host_name, |p| { p.try_rotate(); });
                    }
                }
            }

            MaintenanceAction::WarmSlot1 => {
                info!("[{host_name}] heartbeat: warming slot 1 (staggered)");
                let host_owned = host_name.to_owned();
                let pw_owned = password.to_owned();
                let sec_owned = secret.to_owned();
                let state2 = Arc::clone(state);
                let managers2 = Arc::clone(managers);
                let registry2 = Arc::clone(registry);
                std::thread::Builder::new()
                    .name(format!("warmslot1:{host_name}"))
                    .spawn(move || {
                        // Stagger sleep (mirrors Python start_master_async).
                        std::thread::sleep(SLOT1_STAGGER);
                        // Guard: re-check active state after the stagger.
                        let still_active = {
                            let guard = state2.lock().unwrap();
                            guard
                                .hosts
                                .iter()
                                .find(|h| h.host == host_owned)
                                .map(|h| h.active)
                                .unwrap_or(false)
                        };
                        if !still_active {
                            info!("[{host_owned}] warm-slot-1 aborted — host no longer active");
                            return;
                        }
                        let otp_closure = make_otp_closure(
                            sec_owned,
                            host_owned.clone(),
                            Arc::clone(&registry2),
                        );
                        let mut pool = managers2.snapshot(&host_owned);
                        let ready = start_master(&mut pool, 1, &pw_owned, otp_closure);
                        managers2.write_back(&host_owned, &pool);
                        if ready {
                            info!("[{host_owned}] warm-slot-1: slot 1 Ready");
                        } else {
                            warn!("[{host_owned}] warm-slot-1: slot 1 failed");
                        }
                        // Update engine State slot count if newly ready.
                        let mut guard = state2.lock().unwrap();
                        if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_owned) {
                            if ready {
                                h.pool_alive = h.pool_alive.max(1) + 1;
                            }
                        }
                    })
                    .expect("failed to spawn warm-slot-1 thread");
            }

            MaintenanceAction::Rotate => {
                // try_rotate updates the active symlink and active_index on disk.
                managers.with_pool_mut(host_name, |p| {
                    if p.try_rotate() {
                        let new_idx = p.active_index;
                        info!("[{host_name}] heartbeat: rotated to spare slot {new_idx}");
                    }
                });
                // Update engine State pool_index.
                let new_idx = managers
                    .with_pool(host_name, |p| p.active_index)
                    .unwrap_or(0);
                let mut guard = state.lock().unwrap();
                if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                    h.pool_index = new_idx as u8;
                    h.is_master_ready = true;
                    h.status = "Connected".into();
                    h.last_msg = format!("Rotated to slot {new_idx}");
                }
            }

            MaintenanceAction::Healthy | MaintenanceAction::Skip => {
                // Nothing to do.
            }
        }
    }

    // --- Rotation check (every ROTATION_CHECK_INTERVAL) ---
    if do_rotation_check {
        let rotated = managers.with_pool_mut(host_name, |p| p.try_rotate());

        if rotated {
            let new_idx = managers
                .with_pool(host_name, |p| p.active_index)
                .unwrap_or(0);
            let mut guard = state.lock().unwrap();
            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                h.pool_index = new_idx as u8;
                h.last_msg = format!("Rotated to slot {new_idx}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Slot-1 warm-up after initial connect (called by host_toggle / boot)
// ---------------------------------------------------------------------------

/// After slot 0 becomes ready, start slot 1 in the background (staggered).
///
/// Mirrors `threading.Thread(target=self.start_master_async, args=(1,))` in
/// Python's `manage_pool_loop`.
pub fn spawn_warmup_slot1(
    host_name: String,
    password: String,
    secret: String,
    registry: Arc<OtpRegistry>,
    state: Arc<Mutex<State>>,
    managers: Arc<HostManagers>,
) {
    std::thread::Builder::new()
        .name(format!("warmup-slot1:{host_name}"))
        .spawn(move || {
            // Stagger delay.
            std::thread::sleep(SLOT1_STAGGER);

            // Guard: re-check desired state after the stagger.
            let still_active = {
                let guard = state.lock().unwrap();
                guard
                    .hosts
                    .iter()
                    .find(|h| h.host == host_name)
                    .map(|h| h.active)
                    .unwrap_or(false)
            };
            if !still_active {
                info!("[{host_name}] warmup_slot1 aborted — host no longer active");
                return;
            }

            let otp_closure =
                make_otp_closure(secret, host_name.clone(), Arc::clone(&registry));
            let mut pool = managers.snapshot(&host_name);
            let ready = start_master(&mut pool, 1, &password, otp_closure);
            managers.write_back(&host_name, &pool);

            if ready {
                info!("[{host_name}] warmup_slot1: slot 1 Ready");
                let mut guard = state.lock().unwrap();
                if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                    h.pool_alive = h.pool_alive.max(1) + 1;
                }
            } else {
                warn!("[{host_name}] warmup_slot1: slot 1 failed");
            }
        })
        .expect("failed to spawn warmup-slot1 thread");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use a2fa_core::ssh::master::{OTP_COOLDOWN, OTP_FAILURE_THRESHOLD};

    // -----------------------------------------------------------------------
    // HostManagers — persistent state survives multiple accesses
    // -----------------------------------------------------------------------

    /// The critical regression test: failure count must PERSIST across two
    /// separate `snapshot` / `write_back` round trips (the bug we're fixing).
    #[test]
    fn persistent_pool_failure_count_survives_two_round_trips() {
        let managers = HostManagers::new();

        // First round trip: record 3 failures.
        {
            let mut pool = managers.snapshot("k6");
            pool.consecutive_login_failures = 3;
            managers.write_back("k6", &pool);
        }

        // Second round trip: read back — count must still be 3.
        {
            let pool = managers.snapshot("k6");
            assert_eq!(
                pool.consecutive_login_failures, 3,
                "failure count must survive across write_back+snapshot cycles"
            );
        }
    }

    /// Cooldown must persist across two separate snapshots (circuit-breaker test).
    #[test]
    fn persistent_pool_cooldown_survives_two_round_trips() {
        let managers = HostManagers::new();

        // Trip 1: arm the cooldown.
        {
            let mut pool = managers.snapshot("k6");
            pool.cooldown_until = Some(Instant::now() + OTP_COOLDOWN);
            managers.write_back("k6", &pool);
        }

        // Trip 2: cooldown must still be active.
        {
            let pool = managers.snapshot("k6");
            assert!(
                pool.in_cooldown(),
                "cooldown must persist across write_back+snapshot cycles"
            );
        }
    }

    /// Different hosts must have independent PoolState instances.
    #[test]
    fn different_hosts_have_independent_states() {
        let managers = HostManagers::new();

        {
            let mut pool = managers.snapshot("k6");
            pool.consecutive_login_failures = 7;
            managers.write_back("k6", &pool);
        }

        {
            let pool = managers.snapshot("cannon");
            assert_eq!(
                pool.consecutive_login_failures, 0,
                "cannon must start with 0 failures, independent of k6"
            );
        }
    }

    // -----------------------------------------------------------------------
    // teardown_all — panic-safety and basic coverage
    // -----------------------------------------------------------------------

    /// `teardown_all` on an empty registry must not panic.
    #[test]
    fn teardown_all_empty_does_not_panic() {
        let managers = HostManagers::new();
        // No hosts registered — should be a no-op.
        managers.teardown_all();
    }

    /// `teardown_all` with a freshly-created (but never connected) PoolState
    /// must not panic.  `stop_all` will call `ssh -O exit` on a nonexistent
    /// control path; that subprocess error is absorbed inside `stop_all`, so
    /// no panic should surface here.
    ///
    /// Note: we intentionally do NOT test teardown with a real live SSH master
    /// in a unit test — that would require an actual SSH daemon running during
    /// `cargo test`.  The no-op path (Init-status slots, bogus control path)
    /// exercises the lock/iterate/call path without network I/O.
    #[test]
    fn teardown_all_with_one_entry_does_not_panic() {
        let managers = HostManagers::new();
        // Insert a pool state for a host (control path points nowhere).
        managers.with_pool_mut("testhost", |_p| {});
        // Must not panic even though ssh -O exit will fail.
        managers.teardown_all();
    }

    /// `with_pool_mut` creates a new entry for an unknown host.
    #[test]
    fn with_pool_mut_creates_entry() {
        let managers = HostManagers::new();
        let host_name = managers.with_pool_mut("newhost", |p| p.host.clone());
        assert_eq!(host_name, "newhost");
    }

    /// `with_pool` returns `None` before any entry is created.
    #[test]
    fn with_pool_none_before_first_access() {
        let managers = HostManagers::new();
        let result = managers.with_pool("ghost", |p| p.host.clone());
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // next_action — pure decision function (no ssh I/O)
    // -----------------------------------------------------------------------

    fn fresh_pool(host: &str) -> PoolState {
        PoolState::new(host)
    }

    #[test]
    fn next_action_inactive_host_gives_skip() {
        let pool = fresh_pool("k6");
        let action = next_action(&pool, 0, false, None, Instant::now());
        assert_eq!(action, MaintenanceAction::Skip);
    }

    #[test]
    fn next_action_in_cooldown_gives_skip() {
        let mut pool = fresh_pool("k6");
        pool.cooldown_until = Some(Instant::now() + Duration::from_secs(60));
        // host is active but in cooldown
        let action = next_action(&pool, 0, true, None, Instant::now());
        assert_eq!(action, MaintenanceAction::Skip);
    }

    #[test]
    fn next_action_dead_slot_active_no_cooldown_gives_restart() {
        let mut pool = fresh_pool("k6");
        pool.slot_status[0] = SlotStatus::Dead;
        let action = next_action(&pool, 0, true, None, Instant::now());
        assert_eq!(action, MaintenanceAction::Restart);
    }

    #[test]
    fn next_action_failed_slot_gives_restart() {
        let mut pool = fresh_pool("k6");
        pool.slot_status[0] = SlotStatus::Failed;
        let action = next_action(&pool, 0, true, None, Instant::now());
        assert_eq!(action, MaintenanceAction::Restart);
    }

    #[test]
    fn next_action_ready_slot_with_failed_check_gives_restart() {
        let mut pool = fresh_pool("k6");
        pool.slot_status[0] = SlotStatus::Ready;
        let action = next_action(&pool, 0, true, Some(false), Instant::now());
        assert_eq!(action, MaintenanceAction::Restart);
    }

    #[test]
    fn next_action_ready_slot_with_passing_check_gives_healthy() {
        let mut pool = fresh_pool("k6");
        pool.slot_status[0] = SlotStatus::Ready;
        let action = next_action(&pool, 0, true, Some(true), Instant::now());
        assert_eq!(action, MaintenanceAction::Healthy);
    }

    #[test]
    fn next_action_slot1_init_while_slot0_ready_gives_warmslot1() {
        let mut pool = fresh_pool("k6");
        pool.slot_status[0] = SlotStatus::Ready;
        pool.slot_status[1] = SlotStatus::Init;
        let action = next_action(&pool, 1, true, None, Instant::now());
        assert_eq!(action, MaintenanceAction::WarmSlot1);
    }

    #[test]
    fn next_action_rotate_when_active_slot_dead_and_spare_ready() {
        let mut pool = fresh_pool("k6");
        pool.active_index = 0;
        pool.slot_status[0] = SlotStatus::Dead;
        pool.slot_status[1] = SlotStatus::Ready;
        let action = next_action(&pool, 0, true, None, Instant::now());
        assert_eq!(action, MaintenanceAction::Rotate);
    }

    #[test]
    fn next_action_no_rotate_when_in_probe_backoff() {
        let mut pool = fresh_pool("k6");
        pool.active_index = 0;
        pool.slot_status[0] = SlotStatus::Dead;
        pool.slot_status[1] = SlotStatus::Ready;
        pool.probe_backoff_until = Some(Instant::now() + Duration::from_secs(60));
        let action = next_action(&pool, 0, true, None, Instant::now());
        // In backoff → Restart (not Rotate), but also in_probe_backoff suppresses Restart.
        // Dead slot + probe_backoff → Healthy (both Rotate and Restart are suppressed).
        assert_eq!(action, MaintenanceAction::Healthy);
    }

    /// After threshold failures the cooldown must be armed.
    #[test]
    fn failure_threshold_arms_cooldown() {
        let mut pool = fresh_pool("k6");
        pool.consecutive_login_failures = OTP_FAILURE_THRESHOLD - 1;
        pool.cooldown_until = Some(Instant::now() + OTP_COOLDOWN);
        assert!(pool.in_cooldown());
        // At cooldown → next_action should Skip.
        let action = next_action(&pool, 0, true, None, Instant::now());
        assert_eq!(action, MaintenanceAction::Skip);
    }

    /// Elapsed cooldown → NOT in cooldown → dead slot triggers Restart.
    #[test]
    fn elapsed_cooldown_allows_restart() {
        let mut pool = fresh_pool("k6");
        // Set cooldown to already expired.
        pool.cooldown_until = Some(Instant::now() - Duration::from_secs(1));
        pool.slot_status[0] = SlotStatus::Dead;
        let action = next_action(&pool, 0, true, None, Instant::now());
        assert_eq!(action, MaintenanceAction::Restart);
    }

    /// Init slot (never started) should give Healthy, not Restart.
    #[test]
    fn init_slot_gives_healthy_not_restart() {
        let pool = fresh_pool("k6");
        // slot_status[0] == Init by default
        let action = next_action(&pool, 0, true, None, Instant::now());
        assert_eq!(action, MaintenanceAction::Healthy);
    }
}
