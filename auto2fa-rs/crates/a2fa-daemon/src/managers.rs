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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use log::{info, warn};

use a2fa_core::engine::State;
use a2fa_core::ssh::master::{start_master, stop_all, PoolState, SlotStatus, POOL_SIZE};

use crate::workers::{make_otp_closure, OtpRegistry};

/// Process-lifetime cache of resolved `(password, secret)` per host.
///
/// macOS issues a Keychain "Always Allow" authorization prompt the first time a
/// process reads a protected item. Without this cache the daemon re-reads the
/// Keychain on *every* login attempt (5 call sites), so a host whose login
/// keeps failing — rate-limited cluster, down compute node — re-triggers a
/// prompt on each retry. Across a session that is the observed "countless
/// Keychain prompts" storm. Caching caps Keychain reads at **one per host per
/// daemon lifetime**, no matter how often logins retry.
/// Process credentials cache + a per-host EPOCH counter. The epoch is bumped
/// on every invalidation; a Keychain reader captures the epoch BEFORE its read
/// and refuses to insert if it changed meanwhile — so a slow reader abandoned
/// by the timeout can't resurrect STALE creds after a re-key (the
/// remove→re-add flow, which is exactly when the Keychain is locked and the
/// read times out). Without this, the stale insert is served forever → silent
/// perpetual login failure until daemon restart.
#[derive(Default)]
struct CredsCache {
    creds: HashMap<String, (String, String)>,
    epoch: HashMap<String, u64>,
}

fn creds_cache() -> &'static Mutex<CredsCache> {
    static CACHE: OnceLock<Mutex<CredsCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(CredsCache::default()))
}

/// `true` when both fields are present — only complete creds are worth caching
/// (a partial/failed read must be retried, not cached as the permanent answer).
fn creds_complete(creds: &(String, String)) -> bool {
    !creds.0.is_empty() && !creds.1.is_empty()
}

/// The host's current cache epoch (0 if never invalidated).
fn creds_epoch(host: &str) -> u64 {
    let cache = creds_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.epoch.get(host).copied().unwrap_or(0)
}

/// Drop a host's cached credentials AND bump its epoch. MUST be called whenever
/// the stored creds change (host added / re-keyed / removed) so the next login
/// re-reads the Keychain instead of serving stale secrets. Poison-tolerant.
pub fn invalidate_creds_cache(host: &str) {
    let mut cache = creds_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.creds.remove(host);
    *cache.epoch.entry(host.to_string()).or_insert(0) += 1;
}

/// Read a host's `(password, secret)`, served from the process cache when
/// present and otherwise read once from the macOS Keychain (and cached if
/// complete).
///
/// IMPORTANT: This MUST only ever be called from inside a spawned worker
/// thread, NEVER on the daemon's synchronous startup/accept path or the
/// heartbeat thread. The uncached Keychain read is additionally BOUNDED
/// (sub-worker + 10s recv_timeout): macOS's Security framework serializes
/// Keychain access process-wide, so an unanswered "Always Allow" prompt
/// blocks the reading thread indefinitely — and the caller holds the slot's
/// in-flight StartGuard, so an unbounded read here wedged the slot (and,
/// process-wide, every host's login) forever.
///
/// Missing / unreadable / timed-out creds degrade to empty strings (login
/// then simply fails for that host) rather than propagating an error, and
/// are NOT cached so a later attempt retries the read.
fn load_creds(host: &str) -> (String, String) {
    // Fast path: serve from cache so retries never re-prompt the Keychain.
    {
        let cache = creds_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(creds) = cache.creds.get(host) {
            return creds.clone();
        }
    }
    // Capture the epoch BEFORE the read so a concurrent invalidation (re-key)
    // that lands during the read is detected at insert time.
    let start_epoch = creds_epoch(host);

    // BOUNDED Keychain read: run it on a short-lived sub-worker and join with
    // a hard timeout. The caller (a login/rebuild worker) HOLDS the in-flight
    // StartGuard for its slot — an unbounded SecItem read blocking on a
    // pending SecurityAgent prompt wedged the slot "in flight" FOREVER (the
    // heartbeat logged "restart already in flight, skipping" indefinitely),
    // and macOS serializes Keychain access process-wide so one prompt stalled
    // every host. Timeout -> empty creds -> the login fails loudly, the guard
    // releases, and the heartbeat retries later (re-reading the Keychain,
    // since incomplete creds are never cached). An abandoned sub-worker that
    // completes late writes only into the cache - harmless.
    let creds = {
        let (tx, rx) = std::sync::mpsc::channel();
        let host_owned = host.to_string();
        let spawn_res = std::thread::Builder::new()
            .name(format!("keychain-read:{host}"))
            .spawn(move || {
                use a2fa_core::creds::keychain::KeychainStore;
                use a2fa_core::creds::{get_otpauth, get_password};
                use a2fa_core::totp::extract_secret;

                let ks = KeychainStore;
                let password = get_password(&ks, &host_owned).ok().flatten().unwrap_or_default();
                let otpauth = get_otpauth(&ks, &host_owned).ok().flatten().unwrap_or_default();
                let secret = extract_secret(&otpauth).unwrap_or_default();
                let creds = (password, secret);
                // Cache from HERE so even a timed-out-but-late-completing read
                // benefits the next attempt (complete creds only) — BUT only
                // if no invalidation raced the read (epoch unchanged). A
                // re-key (remove→re-add) bumps the epoch; inserting our
                // now-stale read would serve the OLD password forever.
                if creds_complete(&creds) {
                    let mut cache = creds_cache().lock().unwrap_or_else(|e| e.into_inner());
                    let current = cache.epoch.get(&host_owned).copied().unwrap_or(0);
                    if current == start_epoch {
                        cache.creds.insert(host_owned.clone(), creds.clone());
                    }
                }
                let _ = tx.send(creds);
            });
        match spawn_res {
            Ok(_) => match rx.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok(creds) => creds,
                Err(_) => {
                    warn!(
                        "[{host}] keychain read timed out (locked keychain / pending \
                         prompt?) - this login attempt will fail and retry later"
                    );
                    (String::new(), String::new())
                }
            },
            Err(e) => {
                warn!("[{host}] failed to spawn keychain-read worker: {e}");
                (String::new(), String::new())
            }
        }
    };

    // (Complete creds were cached inside the sub-worker; incomplete ones are
    // never cached so a later attempt retries the Keychain.)
    creds
}

// ---------------------------------------------------------------------------
// Heartbeat / rotation timing constants (mirroring backend.py)
// ---------------------------------------------------------------------------

/// How often the heartbeat loop wakes up.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);

/// How often the rotation / remote-probe check runs.
const ROTATION_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Throttle before restarting a dead master (mirrors Python's `time.sleep(2)`).
const RESTART_THROTTLE: Duration = Duration::from_secs(2);

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
    /// Master is dead / unresponsive — restart it (after throttle).
    Restart,
    /// Registry says Dead/Failed but the LIVE check PASSED: the registry is
    /// stale (boot-adoption gap, a transient check failure that later
    /// recovered, a worker result landing mid-tick). The master is alive and
    /// authenticated — adopt it back to Ready instead of restarting. A restart
    /// here KILLS the live master (cleanup_stale_socket PID-sweeps every [mux]
    /// on the ControlPath) and burns a full 2FA login for nothing. Observed
    /// live: 1246 "needs restart (status=Dead, check=Some(true))" log lines.
    AdoptAlive,
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

    // A Dead/Failed registry status with a PASSING live check means the
    // registry is stale, not the master — adopt, never restart (see
    // MaintenanceAction::AdoptAlive). The master is alive and authenticated; a
    // restart would kill it and burn a full 2FA login for nothing.
    if matches!(pool.slot_status[slot], SlotStatus::Dead | SlotStatus::Failed)
        && check_alive == Some(true)
    {
        return MaintenanceAction::AdoptAlive;
    }

    // Restart if the slot is in a non-Ready state (Dead/Failed), or if the
    // live check returned false. Init on an ACTIVE host also restarts: a rapid
    // toggle OFF→ON race can abandon the activation (the ON arrived while the
    // old stop worker still held the slot token, so the start was skipped) —
    // without this arm no heartbeat path ever started it and the host sat
    // "Connecting" forever.
    let needs_restart = match pool.slot_status[slot] {
        SlotStatus::Dead | SlotStatus::Failed => true,
        SlotStatus::Init => slot == 0,
        SlotStatus::Ready => check_alive == Some(false),
    };

    // Suppress restart while in flap backoff (connect-then-drop) — "stop
    // hammering this host for a while".
    if needs_restart && !pool.in_flap_backoff() {
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
    /// In-flight guard set: the `(host, slot)` pairs that currently have a
    /// master-start / restart worker thread running.
    ///
    /// This is the authoritative fix for the runaway-spawn machine-hang bug.
    /// `start_master` is a blocking ssh+pty login (up to `LOGIN_TIMEOUT` = 60s),
    /// during which the slot stays `Dead`/`Init` in the persistent `PoolState`.
    /// Without this guard, every heartbeat tick (~3s) would see the slot still
    /// not-Ready and spawn *another* login worker — piling up ~20 concurrent
    /// ssh+pty processes per slot and hanging the machine.
    ///
    /// Each spawn site calls [`HostManagers::try_begin_start`] BEFORE spawning;
    /// if a start is already in flight for that `(host, slot)`, it skips. The
    /// worker holds a [`StartGuard`] for its whole lifetime so the entry is
    /// cleared via [`HostManagers::end_start`] on every exit path (normal
    /// return, early return, or panic).
    ///
    /// Lock discipline: this mutex is held only for the brief insert/remove,
    /// NEVER across any ssh I/O.
    starting: Mutex<HashSet<(String, usize)>>,
}

impl HostManagers {
    /// Create a new, empty registry wrapped in an `Arc`.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Try to claim the in-flight start slot for `(host, slot)`.
    ///
    /// Returns `true` if the caller may proceed to spawn a start/restart worker
    /// (no start is already in flight for this exact host+slot), `false` if one
    /// is already running and the caller must skip.
    ///
    /// On `true` the `(host, slot)` is recorded as in-flight; the caller MUST
    /// arrange for [`end_start`](Self::end_start) to be called when the worker
    /// finishes — use a [`StartGuard`] to guarantee this on every exit path.
    pub fn try_begin_start(&self, host: &str, slot: usize) -> bool {
        // HashSet::insert returns true iff the value was NEWLY inserted.
        self.starting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((host.to_owned(), slot))
    }

    /// Release the in-flight start slot for `(host, slot)`.
    pub fn end_start(&self, host: &str, slot: usize) {
        self.starting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(host.to_owned(), slot));
    }

    /// Run `f` with a mutable reference to the `PoolState` for `host`.
    ///
    /// Creates a default `PoolState` if none exists yet.  The lock on the
    /// internal map is held only for the duration of `f` — `f` must be brief
    /// (no ssh I/O, no sleeps).
    pub fn with_pool_mut<R>(&self, host: &str, f: impl FnOnce(&mut PoolState) -> R) -> R {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let pool = map
            .entry(host.to_owned())
            .or_insert_with(|| PoolState::new(host));
        f(pool)
    }

    /// Run `f` with a shared reference to the `PoolState` for `host`.
    ///
    /// Returns `None` if no state exists for this host yet.
    pub fn with_pool<R>(&self, host: &str, f: impl FnOnce(&PoolState) -> R) -> Option<R> {
        let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
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
            slot_ready_since: p.slot_ready_since,
            flap_count: p.flap_count,
            flap_backoff_until: p.flap_backoff_until,
            consecutive_probe_failures: p.consecutive_probe_failures,
        })
    }

    /// Tear down every SSH master in the registry.
    ///
    /// Calls `stop_all` on each `PoolState` in the map (which runs
    /// `ssh -O exit` and cleans up the control-path symlink for each slot).
    /// Errors from individual hosts are logged and swallowed so teardown
    /// continues for the remaining hosts.  Panic-safe.
    ///
    /// Lock discipline: the map lock is held ONLY to snapshot the per-host
    /// `PoolState`s, then DROPPED before the blocking `stop_all` loop. Even
    /// though each `ssh -O exit` is now bounded (~5 s, see
    /// `control::master_exit`), holding the map lock across N×5 s would block
    /// every other map access (heartbeat, host_toggle) for the whole teardown.
    /// The torn-down state is written back under a brief re-acquisition.
    pub fn teardown_all(&self) {
        // 1. Snapshot every host's PoolState under a brief lock, then DROP it.
        let mut pools: Vec<PoolState> = {
            let map = match self.map.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(), // recover from a poisoned lock
            };
            map.values()
                .map(|p| PoolState {
                    host: p.host.clone(),
                    slot_status: p.slot_status,
                    active_index: p.active_index,
                    consecutive_login_failures: p.consecutive_login_failures,
                    cooldown_until: p.cooldown_until,
                    last_rotate: p.last_rotate,
                    probe_backoff_until: p.probe_backoff_until,
                    slot_ready_since: p.slot_ready_since,
                    flap_count: p.flap_count,
                    flap_backoff_until: p.flap_backoff_until,
                    consecutive_probe_failures: p.consecutive_probe_failures,
                })
                .collect()
        };
        let count = pools.len();

        // 2. Run the blocking `stop_all` per host OFF-LOCK. Each `ssh -O exit`
        //    is bounded (~5 s) so a wedged socket can't hang the loop.
        for pool in pools.iter_mut() {
            info!("[{}] teardown_all: stopping all SSH master slots", pool.host);
            // Any subprocess errors are already absorbed inside stop_all.
            stop_all(pool);
        }

        // 3. Re-acquire briefly to write the torn-down state back.
        for pool in &pools {
            self.write_back(&pool.host, pool);
        }
        info!("teardown_all: tore down {count} host(s)");
    }

    /// Whole-pool write-back. ONLY for exclusive whole-pool transitions —
    /// boot adoption, `stop_all` (managed-stop / rebuild phase 1), teardown —
    /// where the writer legitimately owns BOTH slots and `active_index`.
    ///
    /// NEVER call this from a per-slot login worker: a worker's snapshot is up
    /// to ~60 s stale, and copying the whole `slot_status` array clobbers the
    /// OTHER slot's concurrent result (observed: a warmup worker rolled a
    /// just-Ready slot 0 back to `Init`, after which the heartbeat — which maps
    /// `Init` to "never started, don't check" — stopped health-checking the
    /// live master forever). Per-slot workers use [`Self::write_back_slot`].
    pub fn write_back(&self, host: &str, src: &PoolState) {
        self.with_pool_mut(host, |dst| {
            dst.slot_status = src.slot_status;
            dst.slot_ready_since = src.slot_ready_since;
            dst.consecutive_login_failures = src.consecutive_login_failures;
            dst.cooldown_until = src.cooldown_until;
            dst.last_rotate = src.last_rotate;
            dst.probe_backoff_until = src.probe_backoff_until;
            dst.active_index = src.active_index;
        });
    }

    /// Per-slot write-back for login workers: copy ONLY the slot this worker
    /// actually drove (status + ready-since stamp) plus the login circuit
    /// breaker it legitimately updated. Everything else — the other slot,
    /// `active_index`, rotation state, flap counters — is owned by the
    /// heartbeat/rotation logic, which mutates the registry directly under the
    /// lock; overwriting those from a stale snapshot caused lost updates AND
    /// silently disabled the master flap backoff (`slot_ready_since` never
    /// reached the registry, so every drop looked like "never marked Ready").
    pub fn write_back_slot(&self, host: &str, slot: usize, src: &PoolState) {
        self.with_pool_mut(host, |dst| {
            if slot < POOL_SIZE {
                dst.slot_status[slot] = src.slot_status[slot];
                dst.slot_ready_since[slot] = src.slot_ready_since[slot];
            }
            dst.consecutive_login_failures = src.consecutive_login_failures;
            dst.cooldown_until = src.cooldown_until;
        });
    }
}

// ---------------------------------------------------------------------------
// StartGuard — RAII release of the in-flight start guard
// ---------------------------------------------------------------------------

/// RAII guard that releases the `(host, slot)` in-flight start claim when it is
/// dropped — on normal return, early return, OR panic.
///
/// A start worker constructs this as its FIRST action (after the caller has
/// already claimed the slot via [`HostManagers::try_begin_start`]) and holds it
/// for its entire lifetime, so [`HostManagers::end_start`] always runs exactly
/// once when the worker exits by any path.
struct StartGuard {
    managers: Arc<HostManagers>,
    host: String,
    slot: usize,
}

impl Drop for StartGuard {
    fn drop(&mut self) {
        self.managers.end_start(&self.host, self.slot);
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
    // Collect active hosts under the lock.
    let active_hosts: Vec<String> = {
        let guard = crate::lock_state(&state);
        guard
            .hosts
            .iter()
            .filter(|h| h.active)
            .map(|h| h.host.clone())
            .collect()
    };

    for host_name in active_hosts {
        // First: try to ADOPT an already-live master (left by a previous daemon
        // — including the Python daemon during cutover). If a master socket is
        // alive at the resolved ControlPath, take it over without re-logging in.
        // This makes daemon restarts and the Python→Rust handoff zero-2FA.
        let mut pool = managers.snapshot(&host_name);
        if a2fa_core::ssh::master::adopt_if_alive(&mut pool) {
            let idx = pool.active_index;
            managers.write_back(&host_name, &pool);
            // Ensure the active symlink points at the adopted slot. A prior
            // kill+rebuild can leave the base ControlPath (what the user's ssh
            // config resolves to) missing; without it `ssh <host>` can't reach
            // the alive master. Idempotent if it already points here.
            a2fa_core::ssh::control::update_symlink(&host_name, idx);
            // Sweep DUPLICATE masters on every adopted slot: adoption takes
            // the socket OWNER, but older-generation masters on the same
            // path were never targeted by anything (the stale-socket sweep
            // only runs when a slot RESTARTS, and adopted slots don't
            // restart) — kill-9 deploys accumulated one duplicate per
            // deploy, each holding an authenticated cluster connection.
            for slot in 0..POOL_SIZE {
                if pool.slot_status[slot] == SlotStatus::Ready {
                    let p = pool.pool_path(slot);
                    a2fa_core::ssh::control::sweep_duplicate_masters(&p, &host_name);
                }
            }
            let mut guard = crate::lock_state(&state);
            if let Some(h) = guard.hosts.iter_mut().find(|hh| hh.host == host_name) {
                h.is_master_ready = true;
                h.pool_alive = 1;
                h.pool_index = idx as u8;
                h.status = "Connected".into();
                h.last_msg = "Adopted live master (no login)".into();
            }
            info!("[{host_name}] boot: adopted live master slot {idx} — skipping login");
            continue;
        }

        // Update status in State to "Connecting…".
        // NOTE: NO Keychain read happens here — `spawn_managed_start` reads the
        // creds inside its own worker thread, so a blocked "Always Allow"
        // prompt can never wedge the daemon's startup path.
        {
            let mut guard = crate::lock_state(&state);
            if let Some(h) = guard.hosts.iter_mut().find(|hh| hh.host == host_name) {
                h.last_msg = "Boot auto-connecting…".into();
                h.status = "Connecting".into();
            }
        }

        info!("[{host_name}] boot auto-start: spawning master slot 0");
        spawn_managed_start(
            host_name,
            0,
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
///
/// Credentials are read from the Keychain INSIDE the spawned thread (see
/// [`load_creds`]), never on the caller — so a stalled macOS "Always Allow"
/// prompt can only block this one host's worker, never the daemon.
pub fn spawn_managed_start(
    host_name: String,
    slot: usize,
    registry: Arc<OtpRegistry>,
    state: Arc<Mutex<State>>,
    managers: Arc<HostManagers>,
) {
    // In-flight guard: don't spawn if a start/restart is already running for
    // this (host, slot). Prevents runaway login-worker pile-up.
    if !managers.try_begin_start(&host_name, slot) {
        info!("[{host_name}] managed-start: start already in flight for slot {slot}, skipping");
        return;
    }
    let guard_managers = Arc::clone(&managers);
    let guard_host = host_name.clone();
    // Clones for the spawn-Err path (closure consumes `managers`/`host_name`).
    let err_managers = Arc::clone(&managers);
    let err_host = host_name.clone();
    let spawn_res = std::thread::Builder::new()
        .name(format!("managed-start:{host_name}:{slot}"))
        .spawn(move || {
            // RAII: release the in-flight guard on every exit path.
            let _start_guard = StartGuard {
                managers: guard_managers,
                host: guard_host,
                slot,
            };

            // 0. Read Keychain creds IN-THREAD (may block on an unanswered
            //    "Always Allow" prompt — but only this worker is affected).
            let (password, secret) = load_creds(&host_name);

            // 1. Snapshot the current PoolState (brief lock).
            let mut pool = managers.snapshot(&host_name);

            // 2. Build the OTP closure (no locks held).
            let otp_closure = make_otp_closure(secret, host_name.clone(), registry);

            info!("[{host_name}] managed-start worker: slot {slot}");

            // 3. Run start_master — blocking ssh pty, no locks held.
            let ready = start_master(&mut pool, slot, &password, otp_closure);

            // 4. Write ONLY this worker's slot back to the persistent registry.
            managers.write_back_slot(&host_name, slot, &pool);

            // 4b. The user may have toggled the host OFF during the (up to
            // ~60 s) blocking login. Don't resurrect it: stop the fresh master
            // and leave the stopped State alone — otherwise the toggle-off is
            // silently undone and an unmanaged authenticated master leaks.
            let still_active = {
                let guard = crate::lock_state(&state);
                guard
                    .hosts
                    .iter()
                    .find(|h| h.host == host_name)
                    .map(|h| h.active)
                    .unwrap_or(false)
            };
            if !still_active {
                if ready {
                    info!(
                        "[{host_name}] managed-start: host deactivated during login — \
                         stopping the fresh master"
                    );
                    a2fa_core::ssh::master::stop_slot(&mut pool, slot);
                    managers.write_back_slot(&host_name, slot, &pool);
                }
                return;
            }

            // 5. Update engine State.
            let mut guard = crate::lock_state(&state);
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
        });
    if let Err(e) = spawn_res {
        // Transient thread-exhaustion (EAGAIN). The closure (and its StartGuard)
        // never ran, so release the in-flight token here or the (host, slot)
        // would stay wedged for the daemon's life. The heartbeat loop retries.
        warn!("[{err_host}] managed-start: failed to spawn worker thread: {e} — releasing token");
        err_managers.end_start(&err_host, slot);
    }
}

// ---------------------------------------------------------------------------
// spawn_master_rebuild — single-thread stop-then-start (force rebuild)
// ---------------------------------------------------------------------------

/// Spawn ONE thread that force-rebuilds a host's master: tear down every slot,
/// then bring slot 0 back up — guaranteeing stop-before-start within a single
/// thread (mirrors Python `force_master_rebuild`).
///
/// Lock discipline: both the managers map lock and the engine `State` lock are
/// fully released across all blocking ssh I/O (`stop_all`, `start_master`).
/// The pattern is snapshot → off-lock I/O → write_back, repeated for the stop
/// and start phases.
///
/// Credentials are read from the Keychain INSIDE the spawned thread (see
/// [`load_creds`]), never on the caller.
///
/// `reset_breakers`: clear cooldown/flap/probe backoffs before rebuilding.
/// TRUE only for EXPLICIT user actions (reset_all) — the wake_recover path
/// must pass FALSE: NetworkMonitor fires it on every network-up (≥12s
/// apart), and an automatic breaker reset there meant that under an
/// oscillating network the breakers never engaged — fresh full logins every
/// up-phase, burning ~2 codes/min/host indefinitely (the FAS-RC rate-limit
/// incident class).
pub fn spawn_master_rebuild(
    host_name: String,
    registry: Arc<OtpRegistry>,
    state: Arc<Mutex<State>>,
    managers: Arc<HostManagers>,
    reset_breakers: bool,
) {
    // In-flight guard on slot 0 (the slot this rebuild starts). The stop_all
    // phase doesn't need guarding, but holding the guard for the whole worker
    // prevents a concurrent start of slot 0 while this rebuild is in flight.
    if !managers.try_begin_start(&host_name, 0) {
        info!("[{host_name}] master-rebuild: start already in flight for slot 0, skipping");
        return;
    }
    let guard_managers = Arc::clone(&managers);
    let guard_host = host_name.clone();
    // Clones for the spawn-Err path (closure consumes `managers`/`host_name`).
    let err_managers = Arc::clone(&managers);
    let err_host = host_name.clone();
    let spawn_res = std::thread::Builder::new()
        .name(format!("master-rebuild:{host_name}"))
        .spawn(move || {
            // RAII: release the slot-0 in-flight guard on every exit path.
            let _start_guard = StartGuard {
                managers: guard_managers,
                host: guard_host,
                slot: 0,
            };

            // --- Phase 0: read Keychain creds in-thread ---
            let (password, secret) = load_creds(&host_name);

            // --- Phase 1: stop every slot (off-lock) ---
            let mut pool = managers.snapshot(&host_name);
            if reset_breakers {
                pool.reset_circuit_breakers();
            }
            info!("[{host_name}] master-rebuild: stopping all slots");
            stop_all(&mut pool);
            managers.write_back(&host_name, &pool);

            // Reflect the torn-down state in engine State immediately so the UI
            // doesn't show a stale "Connected" while the rebuild is in flight.
            {
                let mut guard = crate::lock_state(&state);
                if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                    h.is_master_ready = false;
                    h.pool_alive = 0;
                    h.status = "Reconnecting".into();
                    h.last_msg = "Master rebuild in progress".into();
                }
            }

            // --- Phase 2: start slot 0 fresh (off-lock) ---
            let mut pool = managers.snapshot(&host_name);
            let otp_closure = make_otp_closure(secret, host_name.clone(), registry);
            info!("[{host_name}] master-rebuild: starting slot 0");
            let ready = start_master(&mut pool, 0, &password, otp_closure);
            // Per-slot write-back: a concurrent slot-1 worker's result must not
            // be clobbered by this snapshot.
            managers.write_back_slot(&host_name, 0, &pool);

            // The user may have toggled the host OFF during the blocking login —
            // don't resurrect it (and don't leak the fresh master).
            let still_active = {
                let guard = crate::lock_state(&state);
                guard
                    .hosts
                    .iter()
                    .find(|h| h.host == host_name)
                    .map(|h| h.active)
                    .unwrap_or(false)
            };
            if !still_active {
                if ready {
                    info!("[{host_name}] master-rebuild: host deactivated during login — stopping fresh master");
                    a2fa_core::ssh::master::stop_slot(&mut pool, 0);
                    managers.write_back_slot(&host_name, 0, &pool);
                }
                return;
            }

            // --- Phase 3: update engine State (mirrors spawn_managed_start) ---
            let mut guard = crate::lock_state(&state);
            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                if ready {
                    h.is_master_ready = true;
                    h.pool_alive = 1;
                    h.pool_index = 0;
                    h.status = "Connected".into();
                    h.last_msg = "Master rebuilt (slot 0 ready)".into();
                    info!("[{host_name}] master-rebuild: slot 0 Ready — State updated");
                } else {
                    h.is_master_ready = false;
                    h.status = if pool.in_cooldown() {
                        "Cooldown".into()
                    } else {
                        "Failed".into()
                    };
                    h.last_msg = "Master rebuild failed (slot 0)".into();
                    warn!("[{host_name}] master-rebuild: slot 0 failed — State updated");
                }
            }
        });
    if let Err(e) = spawn_res {
        // Transient EAGAIN: the closure (and its slot-0 StartGuard) never ran.
        // Release the token so the slot isn't wedged; next manual/auto retry
        // can re-claim it.
        warn!("[{err_host}] master-rebuild: failed to spawn worker thread: {e} — releasing token");
        err_managers.end_start(&err_host, 0);
    }
}

/// Force-rebuild the masters for every host in `hosts` that is `active` in
/// `State`.  Loads credentials per host (same path as `boot_autostart`) and
/// kicks off one [`spawn_master_rebuild`] thread per host.
///
/// Returns the number of rebuilds actually kicked off (i.e. hosts that were
/// both requested AND currently active).
/// `reset_breakers` — see [`spawn_master_rebuild`]: TRUE only for explicit
/// user actions (reset_all); automatic paths (wake_recover) pass FALSE.
pub fn rebuild_masters(
    hosts: &[String],
    state: &Arc<Mutex<State>>,
    managers: &Arc<HostManagers>,
    registry: &Arc<OtpRegistry>,
    reset_breakers: bool,
) -> usize {
    // Filter the requested hosts down to those currently active (brief lock).
    let to_rebuild: Vec<String> = {
        let guard = crate::lock_state(&state);
        hosts
            .iter()
            .filter(|name| {
                guard
                    .hosts
                    .iter()
                    .any(|h| &&h.host == name && h.active)
            })
            .cloned()
            .collect()
    };

    let mut count = 0;
    for host_name in to_rebuild {
        // NOTE: NO Keychain read here — `spawn_master_rebuild` reads creds
        // inside its own worker thread (no-wedge invariant).
        info!("[{host_name}] rebuild_masters: spawning master rebuild");
        spawn_master_rebuild(
            host_name,
            Arc::clone(registry),
            Arc::clone(state),
            Arc::clone(managers),
            reset_breakers,
        );
        count += 1;
    }
    count
}

/// Return the names of every host currently `active` in `State`.
pub fn active_host_names(state: &Arc<Mutex<State>>) -> Vec<String> {
    let guard = crate::lock_state(&state);
    guard
        .hosts
        .iter()
        .filter(|h| h.active)
        .map(|h| h.host.clone())
        .collect()
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

            // 4. Update engine State — but ONLY if the host is still
            //    deactivated. stop_all takes up to ~20s; a user who re-toggles
            //    ON in that window (handler sets active=true + spawns a fresh
            //    login) must not have it silently undone by this late write —
            //    the old unconditional `active=false` made the in-flight ON
            //    login's still_active check fail, killing the master it had
            //    just authenticated (one 2FA burned, State vs disk disagreeing
            //    until reboot).
            let mut guard = crate::lock_state(&state);
            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                if h.active {
                    info!(
                        "[{host_name}] managed-stop: host re-activated during stop — \
                         leaving State to the new activation"
                    );
                } else {
                    h.is_master_ready = false;
                    h.pool_alive = 0;
                    h.active = false;
                    h.status = "Idle".into();
                    h.last_msg = "Stopped".into();
                }
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
    // Degrade, never crash: a transient EAGAIN on spawn must NOT abort the
    // process. .expect() here would panic the main thread → process exit →
    // launchd KeepAlive respawns → boot_autostart re-fires the login spawn
    // wave → the exact spawn-storm/machine-hang feedback loop. Log and continue
    // with auto-reconnect disabled for this process lifetime instead.
    if let Err(e) = std::thread::Builder::new()
        .name("heartbeat".into())
        .spawn(move || heartbeat_loop(state, managers, registry))
    {
        log::error!("failed to spawn heartbeat thread ({e}); auto-reconnect disabled this run");
    }
}

fn heartbeat_loop(
    state: Arc<Mutex<State>>,
    managers: Arc<HostManagers>,
    registry: Arc<OtpRegistry>,
) {
    let mut last_rotation_check = Instant::now();

    loop {
        // Sleep is OUTSIDE the catch_unwind so the loop always paces itself,
        // even if a tick panics.
        std::thread::sleep(HEARTBEAT_INTERVAL);

        // Wrap the whole per-interval heartbeat pass in catch_unwind so a panic
        // in one host's tick is logged and the loop CONTINUES next interval,
        // instead of the heartbeat thread dying and wedging all reconnection.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Snapshot active host NAMES only (brief State lock).
            // CRITICAL no-wedge invariant: do NOT read the Keychain here. The
            // heartbeat thread must never touch the Keychain — a stalled "Always
            // Allow" prompt would otherwise freeze ALL heartbeating. Each restart /
            // warm worker reads its own creds in-thread via `load_creds`.
            let active: Vec<String> = {
                let guard = crate::lock_state(&state);
                guard
                    .hosts
                    .iter()
                    .filter(|h| h.active)
                    .map(|h| h.host.clone())
                    .collect()
            };

            let now = Instant::now();
            let do_rotation_check =
                now.duration_since(last_rotation_check) >= ROTATION_CHECK_INTERVAL;
            if do_rotation_check {
                last_rotation_check = now;
            }

            for host_name in active {
                tick_host(
                    &host_name,
                    do_rotation_check,
                    &state,
                    &managers,
                    &registry,
                );
            }
        }));

        if result.is_err() {
            warn!("heartbeat: a tick panicked — recovered, continuing next interval");
        }
    }
}

/// Decide the host-level State fields after a slot-restart outcome.
///
/// Pure (unit-tested). Key rule: a failed SPARE relogin must NOT repaint a
/// healthy host as failed — observed live (rkempner): slot 1's login timed
/// out and wrote "Reconnect failed" while the ACTIVE slot 0 master was
/// verifiably alive, and nothing on the healthy path ever repaired it.
///
/// Returns `(mark_master_ready, status, last_msg)`.
fn restart_outcome_host_fields(
    slot: usize,
    ready: bool,
    active_index: usize,
    active_ready: bool,
) -> (bool, &'static str, String) {
    if ready {
        (true, "Connected", format!("Slot {slot} reconnected"))
    } else if active_ready && slot != active_index {
        (
            true,
            "Connected",
            format!("Slot {slot} reconnect failed (active slot OK)"),
        )
    } else {
        (
            false,
            "Reconnect failed",
            format!("Slot {slot} reconnect failed"),
        )
    }
}

/// Converge a host's engine-State fields with a VERIFIED-alive active master.
///
/// Called from the heartbeat's Healthy arm: stale writes (e.g. a spare-slot
/// login failure) could leave status "Reconnect failed" / is_master_ready
/// false forever, because the healthy path previously wrote nothing back.
/// Idempotent — re-writing identical values doesn't re-emit events (the tick
/// loop's stable-key change detection sees no difference).
fn heal_host_state(state: &Arc<Mutex<State>>, host: &str, slot: usize, alive_count: u8) {
    let mut guard = crate::lock_state(state);
    if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host) {
        // A host the user just toggled OFF ("Deactivating…", active=false)
        // must not be repainted Connected by a racing tick.
        if !h.active {
            return;
        }
        h.pool_alive = alive_count;
        if !h.is_master_ready || h.status != "Connected" {
            h.is_master_ready = true;
            h.status = "Connected".into();
            h.last_msg = format!("Connected (slot {slot} verified)");
        }
    }
}

/// Run one heartbeat tick for a single host.
///
/// This function is called from the heartbeat loop and is the actual
/// implementation of Python's `manage_pool_loop` inner logic.
fn tick_host(
    host_name: &str,
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
        let mut guard = crate::lock_state(state);
        if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
            h.status = format!("Cooldown ({secs}s)");
        }
        return;
    }

    // --- Per-slot heartbeat: cheap non-blocking liveness probe (off-lock) ---
    for slot in 0..POOL_SIZE {
        let path = pool.pool_path(slot);

        // Only bother probing slots that have ever been started.
        let check_result: Option<bool> = match pool.slot_status[slot] {
            SlotStatus::Init => None, // never started — skip live check
            _ => {
                let liveness = a2fa_core::ssh::control::master_probe(&path);
                // Fold into the per-slot hysteresis counter, then derive the
                // legacy check_alive with the threshold applied. A single Dead
                // probe yields None (no restart); only PROBE_FAILURE_THRESHOLD
                // consecutive Dead probes yield Some(false).
                let failures = managers.with_pool_mut(host_name, |p| {
                    p.note_probe_result(slot, liveness);
                    p.consecutive_probe_failures[slot]
                });
                a2fa_core::ssh::master::probe_to_check(
                    liveness,
                    failures,
                    a2fa_core::ssh::master::PROBE_FAILURE_THRESHOLD,
                )
            }
        };

        // Flap accounting: a Ready slot whose live check passes and that has been
        // up long enough is a STABLE connection → clear any flap state.
        if pool.slot_status[slot] == SlotStatus::Ready && check_result == Some(true) {
            managers.with_pool_mut(host_name, |p| p.note_slot_alive(slot));
        }

        let action = next_action(&pool, slot, true, check_result, now);

        match action {
            MaintenanceAction::Restart => {
                warn!(
                    "[{host_name}] heartbeat: slot {slot} needs restart (status={:?}, check={:?})",
                    pool.slot_status[slot], check_result
                );
                // A Ready slot whose check failed is a DROP — feed flap detection
                // (a short-lived connection counts toward the flap back-off).
                let was_ready_drop =
                    pool.slot_status[slot] == SlotStatus::Ready && check_result == Some(false);
                // Mark slot Dead in the persistent registry.
                managers.with_pool_mut(host_name, |p| {
                    if was_ready_drop {
                        p.note_slot_dropped(slot);
                    }
                    p.slot_status[slot] = SlotStatus::Dead;
                });
                // Update engine State.
                {
                    let mut guard = crate::lock_state(state);
                    if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                        if slot == h.pool_index as usize {
                            h.is_master_ready = false;
                            h.status = "Reconnecting".into();
                            h.last_msg = format!("Slot {slot} dead — reconnecting");
                        }
                    }
                }

                // In-flight guard: if a restart worker is already running for
                // this (host, slot), do NOT spawn another. This is the fix for
                // the machine-hang bug: without it, the slot stays Dead for the
                // whole 60s login and every 3s tick would pile on another
                // ssh+pty login worker.
                if !managers.try_begin_start(host_name, slot) {
                    info!(
                        "[{host_name}] heartbeat: restart already in flight for slot {slot}, skipping spawn"
                    );
                    continue;
                }

                // The throttle, Keychain read, and blocking restart all run on
                // a dedicated worker thread — NEVER on the heartbeat thread.
                // This keeps the no-wedge invariant: a stalled "Always Allow"
                // prompt blocks only this one restart worker, and the heartbeat
                // keeps probing every other host.
                let host_owned = host_name.to_owned();
                let active_index = pool.active_index;
                let state2 = Arc::clone(state);
                let managers2 = Arc::clone(managers);
                let registry2 = Arc::clone(registry);
                let guard_managers = Arc::clone(managers);
                let guard_host = host_name.to_owned();
                let spawn_res = std::thread::Builder::new()
                    .name(format!("hb-restart:{host_name}:{slot}"))
                    .spawn(move || {
                        // RAII: release the in-flight guard on every exit path
                        // (including the early-return when host is deactivated
                        // during the throttle, and on panic).
                        let _start_guard = StartGuard {
                            managers: guard_managers,
                            host: guard_host,
                            slot,
                        };

                        // Throttle before restart (mirrors Python's time.sleep(2)).
                        std::thread::sleep(RESTART_THROTTLE);

                        // Re-check active flag — host may have been toggled off
                        // during the throttle.
                        let still_active = {
                            let guard = crate::lock_state(&state2);
                            guard
                                .hosts
                                .iter()
                                .find(|h| h.host == host_owned)
                                .map(|h| h.active)
                                .unwrap_or(false)
                        };
                        if !still_active {
                            info!("[{host_owned}] heartbeat: host deactivated during throttle — skipping restart");
                            return;
                        }

                        // Adopt-before-restart: re-probe RIGHT before the
                        // destructive restart. If a master is listening now (it
                        // came back, or never actually died — a probe blip / load
                        // spike), DO NOT kill it and DO NOT burn a 2FA login.
                        // Adopt it back to Ready. This is the gate that makes a
                        // false condemnation non-destructive.
                        {
                            let path = managers2.snapshot(&host_owned).pool_path(slot);
                            if a2fa_core::ssh::control::master_probe(&path)
                                == a2fa_core::ssh::control::MasterLiveness::Alive
                            {
                                info!("[{host_owned}] hb-restart: master ALIVE on re-probe — adopting (no kill, no 2FA)");
                                managers2.with_pool_mut(&host_owned, |p| {
                                    p.slot_status[slot] = SlotStatus::Ready;
                                    p.consecutive_probe_failures[slot] = 0;
                                    p.mark_slot_ready(slot);
                                });
                                let alive = managers2
                                    .with_pool(&host_owned, |p| {
                                        p.slot_status
                                            .iter()
                                            .filter(|s| **s == SlotStatus::Ready)
                                            .count() as u8
                                    })
                                    .unwrap_or(1);
                                heal_host_state(&state2, &host_owned, slot, alive);
                                return; // StartGuard drops → releases the in-flight token
                            }
                        }

                        // Read Keychain creds IN-THREAD (may block on a prompt).
                        let (password, secret) = load_creds(&host_owned);

                        // Restart off-lock.
                        let otp_closure = make_otp_closure(
                            secret,
                            host_owned.clone(),
                            Arc::clone(&registry2),
                        );
                        let mut pool_mut = managers2.snapshot(&host_owned);
                        let ready = start_master(&mut pool_mut, slot, &password, otp_closure);
                        // Per-slot write-back — never clobber the other slot /
                        // rotation state from this (stale) snapshot.
                        managers2.write_back_slot(&host_owned, slot, &pool_mut);

                        // Toggle-off during the blocking login → don't resurrect.
                        let still_active2 = {
                            let guard = crate::lock_state(&state2);
                            guard
                                .hosts
                                .iter()
                                .find(|h| h.host == host_owned)
                                .map(|h| h.active)
                                .unwrap_or(false)
                        };
                        if !still_active2 {
                            if ready {
                                info!("[{host_owned}] hb-restart: host deactivated during login — stopping fresh master");
                                a2fa_core::ssh::master::stop_slot(&mut pool_mut, slot);
                                managers2.write_back_slot(&host_owned, slot, &pool_mut);
                            }
                            return;
                        }

                        // Write result back to engine State. pool_index follows
                        // the ACTIVE slot only (restarting the spare must not
                        // repoint the UI), and pool_alive is the real Ready
                        // count (not a hardcoded 1 that clobbers a 2).
                        let (alive_count, reg_active_idx, active_ready) = managers2
                            .with_pool(&host_owned, |p| {
                                let alive = p
                                    .slot_status
                                    .iter()
                                    .filter(|s| **s == SlotStatus::Ready)
                                    .count() as u8;
                                (
                                    alive,
                                    p.active_index,
                                    p.slot_status[p.active_index] == SlotStatus::Ready,
                                )
                            })
                            .unwrap_or((u8::from(ready), active_index, false));
                        {
                            // restart_outcome_host_fields: a failed SPARE
                            // relogin must not repaint a healthy host as
                            // failed when the active master is fine.
                            let (mark_ready, status, last_msg) = restart_outcome_host_fields(
                                slot,
                                ready,
                                reg_active_idx,
                                active_ready,
                            );
                            let mut guard = crate::lock_state(&state2);
                            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_owned) {
                                if mark_ready {
                                    h.is_master_ready = true;
                                    h.pool_alive = alive_count;
                                    if ready && slot == active_index {
                                        h.pool_index = slot as u8;
                                    }
                                }
                                h.status = status.into();
                                h.last_msg = last_msg;
                            }
                        }
                    });
                if let Err(e) = spawn_res {
                    // Transient EAGAIN: the closure (and its StartGuard) never
                    // ran. Release the in-flight token so the slot isn't wedged
                    // Dead forever — the next heartbeat tick will retry.
                    warn!(
                        "[{host_name}] heartbeat: failed to spawn hb-restart thread for slot {slot}: {e} — releasing token"
                    );
                    managers.end_start(host_name, slot);
                    continue;
                }
            }

            MaintenanceAction::AdoptAlive => {
                info!(
                    "[{host_name}] heartbeat: slot {slot} marked {:?} but master is ALIVE — adopting (no restart, no 2FA)",
                    pool.slot_status[slot]
                );
                managers.with_pool_mut(host_name, |p| {
                    p.slot_status[slot] = SlotStatus::Ready;
                    // Stamp the flap-detection clock so a later genuine drop
                    // is accounted correctly.
                    p.mark_slot_ready(slot);
                });
                // Same duplicate sweep as boot adoption: we just adopted the
                // socket owner — evict any older-generation master still
                // squatting on this path.
                a2fa_core::ssh::control::sweep_duplicate_masters(&pool.pool_path(slot), host_name);
                // Reflect in engine State: count Ready slots; mark Connected
                // if this is the active slot.
                let (alive_count, is_active_slot) = managers
                    .with_pool(host_name, |p| {
                        let alive = p
                            .slot_status
                            .iter()
                            .filter(|s| **s == SlotStatus::Ready)
                            .count() as u8;
                        (alive, p.active_index == slot)
                    })
                    .unwrap_or((1, false));
                let mut guard = crate::lock_state(state);
                if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                    h.pool_alive = alive_count;
                    if is_active_slot {
                        h.is_master_ready = true;
                        h.status = "Connected".into();
                        h.last_msg = format!("Adopted live slot {slot}");
                    }
                }
            }

            MaintenanceAction::Healthy => {
                // Converge engine State with the verified registry truth, but
                // only off the ACTIVE slot's PASSING live check. A failed
                // spare-slot relogin (or any stale write) could leave the
                // host-level status "Reconnect failed" / is_master_ready =
                // false while the active master is demonstrably alive —
                // observed live (rkempner): the UI showed a failed host
                // indefinitely because the healthy path wrote nothing back.
                if slot == pool.active_index && check_result == Some(true) {
                    let alive_count = managers
                        .with_pool(host_name, |p| {
                            p.slot_status
                                .iter()
                                .filter(|s| **s == SlotStatus::Ready)
                                .count() as u8
                        })
                        .unwrap_or(1);
                    heal_host_state(state, host_name, slot, alive_count);
                }
            }

            MaintenanceAction::Skip => {
                // Nothing to do.
            }
        }
    }

    // Single-master: no rotation. `do_rotation_check` is retained in the
    // signature for the tick scheduler but is now a no-op.
    let _ = do_rotation_check;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use a2fa_core::ssh::master::{OTP_COOLDOWN, OTP_FAILURE_THRESHOLD};

    // -----------------------------------------------------------------------
    // Credential cache — caps Keychain reads at one per host per lifetime
    // -----------------------------------------------------------------------

    #[test]
    fn creds_complete_requires_both_fields() {
        assert!(creds_complete(&("pw".into(), "secret".into())));
        assert!(!creds_complete(&("".into(), "secret".into())));
        assert!(!creds_complete(&("pw".into(), "".into())));
        assert!(!creds_complete(&("".into(), "".into())));
    }

    #[test]
    fn load_creds_serves_from_cache_without_touching_keychain() {
        // Pre-populate the cache for a unique host; load_creds must return the
        // cached value on the fast path (a Keychain read for this fake host
        // would yield empty, so a non-empty result proves the cache was used).
        let host = "cache-hit-test-host-zzz1";
        {
            let mut c = creds_cache().lock().unwrap_or_else(|e| e.into_inner());
            c.creds.insert(host.to_string(), ("cached-pw".into(), "cached-secret".into()));
        }
        let got = load_creds(host);
        assert_eq!(got, ("cached-pw".to_string(), "cached-secret".to_string()));
        invalidate_creds_cache(host);
    }

    #[test]
    fn invalidate_creds_cache_removes_entry() {
        let host = "cache-invalidate-test-host-zzz2";
        {
            let mut c = creds_cache().lock().unwrap_or_else(|e| e.into_inner());
            c.creds.insert(host.to_string(), ("p".into(), "s".into()));
        }
        invalidate_creds_cache(host);
        let c = creds_cache().lock().unwrap_or_else(|e| e.into_inner());
        assert!(!c.creds.contains_key(host), "entry must be gone after invalidate");
    }

    /// REGRESSION: an insert carrying a STALE epoch (a slow reader that
    /// started before a re-key) must be rejected — otherwise the old password
    /// is served forever. Simulate by bumping the epoch (invalidate) AFTER
    /// capturing the start epoch, then attempting the guarded insert.
    #[test]
    fn epoch_guard_rejects_stale_insert() {
        let host = "epoch-guard-host-zzz9";
        invalidate_creds_cache(host); // clean slate
        let start = creds_epoch(host);
        // A re-key lands while our "read" is in flight.
        invalidate_creds_cache(host);
        // Guarded insert (mirrors the load_creds sub-worker) must NO-OP.
        {
            let mut cache = creds_cache().lock().unwrap_or_else(|e| e.into_inner());
            let current = cache.epoch.get(host).copied().unwrap_or(0);
            if current == start {
                cache.creds.insert(host.to_string(), ("stale".into(), "stale".into()));
            }
        }
        let c = creds_cache().lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            !c.creds.contains_key(host),
            "a stale-epoch insert must be rejected (no resurrecting old creds)"
        );
    }

    #[test]
    fn load_creds_does_not_cache_empty_results() {
        // A host with no Keychain entry resolves to empties (no prompt for a
        // nonexistent item) and must NOT be cached, so a later add+login retries.
        let host = "definitely-nonexistent-host-zzz3-auto2fa-test";
        invalidate_creds_cache(host);
        let got = load_creds(host);
        assert_eq!(got, (String::new(), String::new()));
        let c = creds_cache().lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            !c.creds.contains_key(host),
            "incomplete/empty creds must not be cached (must retry later)"
        );
    }

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
    fn next_action_dead_slot_with_passing_check_adopts_instead_of_restart() {
        // Registry says Dead but the live check PASSES → the registry is
        // stale, the master is alive and authenticated. Restarting would kill
        // it and burn a 2FA login (observed 1246x live) — must adopt.
        for status in [SlotStatus::Dead, SlotStatus::Failed] {
            let mut pool = fresh_pool("k6");
            pool.slot_status[0] = status;
            let action = next_action(&pool, 0, true, Some(true), Instant::now());
            assert_eq!(
                action,
                MaintenanceAction::AdoptAlive,
                "{status:?}+alive-check must adopt, not restart"
            );
        }
        // No check info (None) keeps the restart behavior.
        let mut pool = fresh_pool("k6");
        pool.slot_status[0] = SlotStatus::Dead;
        assert_eq!(
            next_action(&pool, 0, true, None, Instant::now()),
            MaintenanceAction::Restart
        );
    }

    #[test]
    fn next_action_ready_slot_with_passing_check_gives_healthy() {
        let mut pool = fresh_pool("k6");
        pool.slot_status[0] = SlotStatus::Ready;
        let action = next_action(&pool, 0, true, Some(true), Instant::now());
        assert_eq!(action, MaintenanceAction::Healthy);
    }

    /// REGRESSION (rkempner, observed live): a failed SPARE relogin must not
    /// repaint a healthy host as failed when the active master is Ready.
    #[test]
    fn restart_outcome_spare_failure_keeps_host_connected() {
        // Spare slot 1 failed, active slot 0 Ready → host stays Connected.
        let (mark_ready, status, msg) = restart_outcome_host_fields(1, false, 0, true);
        assert!(mark_ready);
        assert_eq!(status, "Connected");
        assert!(msg.contains("active slot OK"), "msg: {msg}");

        // Spare failed AND active not Ready → genuine failure.
        let (mark_ready, status, _) = restart_outcome_host_fields(1, false, 0, false);
        assert!(!mark_ready);
        assert_eq!(status, "Reconnect failed");

        // ACTIVE slot itself failed → failure even if the registry claims the
        // active slot Ready (it just failed — registry info is the past).
        let (mark_ready, status, _) = restart_outcome_host_fields(0, false, 0, true);
        assert!(!mark_ready);
        assert_eq!(status, "Reconnect failed");

        // Success path unchanged.
        let (mark_ready, status, msg) = restart_outcome_host_fields(0, true, 0, true);
        assert!(mark_ready);
        assert_eq!(status, "Connected");
        assert!(msg.contains("reconnected"));
    }

    /// REGRESSION (rkempner, observed live): the Healthy arm must repair
    /// stale host-level failure status once the active master verifies alive
    /// — previously the healthy path wrote nothing and "Reconnect failed"
    /// stuck forever despite pool_alive=2.
    #[test]
    fn heal_host_state_repairs_stale_failure_status() {
        let mut st = State::with_tunnels(vec![]);
        st.hosts.push(a2fa_core::model::Host {
            host: "rk".into(),
            status: "Reconnect failed".into(),
            active: true,
            is_master_ready: false,
            pool_index: 0,
            pool_alive: 0,
            is_mounted: false,
            last_msg: "Slot 1 reconnect failed".into(),
        });
        let state = Arc::new(Mutex::new(st));

        heal_host_state(&state, "rk", 0, 2);

        let guard = crate::lock_state(&state);
        let h = &guard.hosts[0];
        assert!(h.is_master_ready, "must repair is_master_ready");
        assert_eq!(h.status, "Connected");
        assert_eq!(h.pool_alive, 2);

        // Idempotent: a second heal doesn't churn last_msg once Connected.
        drop(guard);
        {
            let mut g = crate::lock_state(&state);
            g.hosts[0].last_msg = "via k6".into();
        }
        heal_host_state(&state, "rk", 0, 2);
        let guard = crate::lock_state(&state);
        assert_eq!(guard.hosts[0].last_msg, "via k6", "no churn when already Connected");
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
    fn init_slot_zero_on_active_host_restarts() {
        // REGRESSION (rapid toggle OFF->ON): an ACTIVE host whose slot 0 is
        // still Init was previously Healthy -- i.e. NO heartbeat arm ever
        // started it, and a toggle-race-abandoned activation sat
        // "Connecting" forever. Slot 0 Init must restart.
        let pool = fresh_pool("k6");
        // slot_status[0] == Init by default
        let action = next_action(&pool, 0, true, None, Instant::now());
        assert_eq!(action, MaintenanceAction::Restart);
    }

    /// REGRESSION (rapid toggle OFF->ON): the late stop-worker State write
    /// must NOT clobber a host the user has re-activated meanwhile.
    #[test]
    fn managed_stop_spares_reactivated_host() {
        let mut st = State::with_tunnels(vec![]);
        let mut h = host("k6", true); // re-toggled ON during the stop
        h.status = "Connecting...".into();
        h.last_msg = "Connecting...".into();
        st.hosts.push(h);
        let state = Arc::new(Mutex::new(st));
        let managers = HostManagers::new();

        spawn_managed_stop("k6".into(), Arc::clone(&state), Arc::clone(&managers));

        // The stop worker runs stop_all on an Init pool (fast no-op) then
        // writes State. Give it a moment to finish.
        std::thread::sleep(std::time::Duration::from_secs(2));

        let guard = crate::lock_state(&state);
        let h = &guard.hosts[0];
        assert!(h.active, "active flag must survive the late stop write");
        assert_eq!(h.status, "Connecting...", "status must be left to the new activation");
        assert_ne!(h.last_msg, "Stopped", "stop worker must not claim it stopped the host");
    }

    // -----------------------------------------------------------------------
    // active_host_names / rebuild_masters — pure selection logic (no ssh I/O)
    // -----------------------------------------------------------------------

    use a2fa_core::model::Host;

    fn host(name: &str, active: bool) -> Host {
        Host {
            host: name.into(),
            status: "Idle".into(),
            active,
            is_master_ready: false,
            pool_index: 0,
            pool_alive: 0,
            is_mounted: false,
            last_msg: String::new(),
        }
    }

    fn state_with_hosts(hosts: Vec<Host>) -> Arc<Mutex<State>> {
        let mut s = State::with_tunnels(vec![]);
        s.hosts = hosts;
        Arc::new(Mutex::new(s))
    }

    #[test]
    fn active_host_names_returns_only_active() {
        let state = state_with_hosts(vec![
            host("k6", true),
            host("cannon", false),
            host("holy", true),
        ]);
        let names = active_host_names(&state);
        assert_eq!(names, vec!["k6".to_string(), "holy".to_string()]);
    }

    #[test]
    fn active_host_names_empty_when_no_hosts() {
        let state = state_with_hosts(vec![]);
        assert!(active_host_names(&state).is_empty());
    }

    /// `rebuild_masters` with an empty host list must do nothing and return 0
    /// (no ssh, no panic).
    #[test]
    fn rebuild_masters_empty_list_returns_zero() {
        let state = state_with_hosts(vec![host("k6", true)]);
        let managers = HostManagers::new();
        let registry = OtpRegistry::new();
        let n = rebuild_masters(&[], &state, &managers, &registry, false);
        assert_eq!(n, 0);
    }

    /// No-wedge guarantee: `boot_autostart` must return promptly and without
    /// panicking even when every active host needs a login.  It must NOT read
    /// the Keychain on the calling thread — credential reads happen inside the
    /// spawned `spawn_managed_start` worker threads (which we don't join here).
    /// The control paths are bogus, so `adopt_if_alive` returns false and each
    /// host takes the spawn path; the call returns immediately regardless.
    #[test]
    fn boot_autostart_returns_promptly_with_active_hosts() {
        let state = state_with_hosts(vec![
            host("k6", true),
            host("cannon", true),
            host("idlehost", false),
        ]);
        let managers = HostManagers::new();
        let registry = OtpRegistry::new();

        let start = Instant::now();
        boot_autostart(&state, &managers, &registry);
        // The calling thread must not block on any Keychain / ssh I/O.
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "boot_autostart must return promptly (no creds/ssh on the caller)"
        );

        // Active hosts were moved to "Connecting"; inactive host untouched.
        let guard = state.lock().unwrap();
        let k6 = guard.hosts.iter().find(|h| h.host == "k6").unwrap();
        assert_eq!(k6.status, "Connecting");
        let idle = guard.hosts.iter().find(|h| h.host == "idlehost").unwrap();
        assert_eq!(idle.status, "Idle");
    }

    // -----------------------------------------------------------------------
    // In-flight start guard — the machine-hang fix
    // -----------------------------------------------------------------------

    /// `try_begin_start` returns true the first time for a (host, slot) and
    /// false on a second attempt (a start is in flight). After `end_start` it
    /// returns true again.
    #[test]
    fn try_begin_start_blocks_second_concurrent_start() {
        let managers = HostManagers::new();

        // First claim succeeds.
        assert!(managers.try_begin_start("k6", 0));
        // Second claim while the first is in flight is rejected.
        assert!(!managers.try_begin_start("k6", 0));
        // A different slot on the same host is independent.
        assert!(managers.try_begin_start("k6", 1));
        // A different host is independent.
        assert!(managers.try_begin_start("cannon", 0));

        // Releasing (k6, 0) lets it be claimed again.
        managers.end_start("k6", 0);
        assert!(managers.try_begin_start("k6", 0));
    }

    /// `end_start` on a key that isn't in flight is a harmless no-op.
    #[test]
    fn end_start_on_unclaimed_key_is_noop() {
        let managers = HostManagers::new();
        managers.end_start("ghost", 0); // must not panic
        assert!(managers.try_begin_start("ghost", 0));
    }

    /// Dropping a `StartGuard` releases the in-flight claim, so the same
    /// (host, slot) can be claimed again afterwards. This is the RAII property
    /// that guarantees release on every worker exit path (incl. panic).
    #[test]
    fn start_guard_drop_releases_claim() {
        let managers = HostManagers::new();

        assert!(managers.try_begin_start("k6", 0));
        {
            let _guard = StartGuard {
                managers: Arc::clone(&managers),
                host: "k6".to_string(),
                slot: 0,
            };
            // Still in flight inside the scope.
            assert!(!managers.try_begin_start("k6", 0));
        }
        // Guard dropped at end of scope → claim released → claimable again.
        assert!(
            managers.try_begin_start("k6", 0),
            "StartGuard drop must call end_start"
        );
    }

    /// `rebuild_masters` filters out inactive hosts — passing an inactive host
    /// name kicks off nothing.
    #[test]
    fn rebuild_masters_filters_inactive_hosts() {
        let state = state_with_hosts(vec![host("cannon", false)]);
        let managers = HostManagers::new();
        let registry = OtpRegistry::new();
        let n = rebuild_masters(&["cannon".to_string()], &state, &managers, &registry, false);
        assert_eq!(n, 0, "inactive host must not be rebuilt");
    }
}
