//! SSH ControlMaster pool — Rust port of `SSHHostManager` in `backend.py`.
//!
//! # Architecture notes
//!
//! * Pool size is 2 (POOL_SIZE = 2), same as the Python daemon.
//! * `start_master` builds the exact same `ssh` argv as `_start_master_impl`
//!   in Python, calls `pty_auth::run_login`, and on success records the slot
//!   as Ready; on repeated failure it arms a cooldown.
//! * OTP serialization / replay-guard is the CALLER'S responsibility. The
//!   `otp_provider` closure is already guarded before it is passed in. This
//!   matches the Python design where the OTP lock is acquired before calling
//!   `_start_master_impl` (via `_fresh_otp_or_wait` + the per-group lock).
//! * Cooldown and probe back-off state live on `PoolState`, which the engine
//!   layer should hold per host.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use log::{info, warn};

use crate::error::Result;
use crate::ssh::control;
use crate::ssh::failure::failure_reason;
use crate::ssh::pty_auth::{run_login, LoginOutcome};

// ---------------------------------------------------------------------------
// Constants (mirroring backend.py)
// ---------------------------------------------------------------------------

/// Number of pool slots per host.
pub const POOL_SIZE: usize = 2;

/// How many consecutive login failures before entering cooldown.
pub const OTP_FAILURE_THRESHOLD: u32 = 5;

/// How long to sit out when rate-limit cooldown is triggered.
pub const OTP_COOLDOWN: Duration = Duration::from_secs(60);

/// A master that stays Ready at least this long counts as a STABLE connection
/// (clears the flap counter). A Ready slot that dies sooner than this is a
/// "flap" (connect-then-drop).
pub const FLAP_MIN_UPTIME: Duration = Duration::from_secs(30);

/// Consecutive flaps before arming the flap back-off.
pub const FLAP_THRESHOLD: u32 = 4;

/// Consecutive confident "dead" probes before a Ready master is condemned.
/// One transient probe failure must never trigger a reconnect.
pub const PROBE_FAILURE_THRESHOLD: u32 = 3;

/// How long to back off restarting a slot that keeps flapping. Stops a host
/// that authenticates then immediately drops from being reconnected (full 2FA)
/// every few seconds forever — the connect-then-drop case the login-failure
/// breaker never caught (it resets on every successful login).
pub const FLAP_BACKOFF: Duration = Duration::from_secs(60);

/// Ping-pong window: if we rotate twice within this window, back off.
pub const ROTATION_PING_PONG_WINDOW: Duration = Duration::from_secs(30);

/// How long to back off when ping-pong is detected.
pub const PROBE_BACKOFF: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Pool status per slot
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlotStatus {
    #[default]
    Init,
    Ready,
    Dead,
    Failed,
}

// ---------------------------------------------------------------------------
// Pool state (held per-host by the engine)
// ---------------------------------------------------------------------------

/// All runtime state for a single host's ControlMaster pool.
///
/// Instantiate once per host; reuse across `start_master` calls.
pub struct PoolState {
    pub host: String,
    pub slot_status: [SlotStatus; POOL_SIZE],
    pub active_index: usize,

    // Cooldown after N consecutive login failures
    pub consecutive_login_failures: u32,
    pub cooldown_until: Option<Instant>,

    // Rotation ping-pong detection
    pub last_rotate: Option<Instant>,
    pub probe_backoff_until: Option<Instant>,

    // Flap detection (connect-then-drop): when each slot became Ready, a count
    // of consecutive short-lived connections, and a back-off once they pile up.
    pub slot_ready_since: [Option<Instant>; POOL_SIZE],
    pub flap_count: u32,
    pub flap_backoff_until: Option<Instant>,

    /// Consecutive confident `Dead` probe results per slot (hysteresis). Reset
    /// to 0 on any `Alive`; untouched by `Inconclusive`.
    pub consecutive_probe_failures: [u32; POOL_SIZE],
}

impl PoolState {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            slot_status: Default::default(),
            active_index: 0,
            consecutive_login_failures: 0,
            cooldown_until: None,
            last_rotate: None,
            probe_backoff_until: None,
            slot_ready_since: [None; POOL_SIZE],
            flap_count: 0,
            flap_backoff_until: None,
            consecutive_probe_failures: [0; POOL_SIZE],
        }
    }

    /// True if we are currently sitting out a cooldown period.
    pub fn in_cooldown(&self) -> bool {
        self.cooldown_until
            .map(|t| Instant::now() < t)
            .unwrap_or(false)
    }

    /// True if probe back-off is active (both slots failing → ping-pong).
    pub fn in_probe_backoff(&self) -> bool {
        self.probe_backoff_until
            .map(|t| Instant::now() < t)
            .unwrap_or(false)
    }

    /// True while a flap (connect-then-drop) back-off is active — the heartbeat
    /// must NOT restart the slot during this window.
    pub fn in_flap_backoff(&self) -> bool {
        self.flap_backoff_until
            .map(|t| Instant::now() < t)
            .unwrap_or(false)
    }

    /// Record that a slot just became Ready (login success). Starts its uptime
    /// clock for flap detection.
    pub fn mark_slot_ready(&mut self, index: usize) {
        if index < POOL_SIZE {
            self.slot_ready_since[index] = Some(Instant::now());
        }
    }

    /// A Ready slot's live check just PASSED. If it has now been up at least
    /// [`FLAP_MIN_UPTIME`], the connection is proven stable → clear flap state.
    pub fn note_slot_alive(&mut self, index: usize) {
        if index >= POOL_SIZE {
            return;
        }
        if let Some(since) = self.slot_ready_since[index] {
            if since.elapsed() >= FLAP_MIN_UPTIME
                && (self.flap_count > 0 || self.flap_backoff_until.is_some())
            {
                self.flap_count = 0;
                self.flap_backoff_until = None;
            }
        }
    }

    /// Fold one probe result into the per-slot hysteresis counter.
    pub fn note_probe_result(&mut self, index: usize, liveness: crate::ssh::control::MasterLiveness) {
        use crate::ssh::control::MasterLiveness::*;
        if index >= POOL_SIZE {
            return;
        }
        match liveness {
            Alive => self.consecutive_probe_failures[index] = 0,
            Dead => self.consecutive_probe_failures[index] += 1,
            Inconclusive => {} // no confident answer — leave the counter alone
        }
    }

    /// A Ready slot was found DROPPED. A drop after < [`FLAP_MIN_UPTIME`] is a
    /// flap; [`FLAP_THRESHOLD`] consecutive flaps arm a [`FLAP_BACKOFF`] so a
    /// host that connects-then-drops isn't reconnected (full 2FA) every few
    /// seconds forever. A drop after a long stable run is NOT a flap (it resets
    /// the counter). The login-failure breaker never caught this because it
    /// resets `consecutive_login_failures` on every successful login.
    pub fn note_slot_dropped(&mut self, index: usize) {
        if index >= POOL_SIZE {
            return;
        }
        match self.slot_ready_since[index].take().map(|t| t.elapsed()) {
            Some(uptime) if uptime < FLAP_MIN_UPTIME => {
                self.flap_count += 1;
                if self.flap_count >= FLAP_THRESHOLD {
                    self.flap_backoff_until = Some(Instant::now() + FLAP_BACKOFF);
                    warn!(
                        "[{}] {} short-lived connections (flapping) — backing off {}s",
                        self.host,
                        self.flap_count,
                        FLAP_BACKOFF.as_secs()
                    );
                }
            }
            _ => {
                // Long-lived connection died (or never marked Ready) — not a flap.
                self.flap_count = 0;
            }
        }
    }

    /// Record one failed login attempt against the circuit breaker: bump the
    /// consecutive-failure counter and arm the cooldown once it crosses
    /// [`OTP_FAILURE_THRESHOLD`]. Shared by the `AuthFailed` and `Err`
    /// (totp/system error) arms of [`start_master`] so a permanently-bad secret
    /// can't be re-driven by the heartbeat every cycle forever.
    pub fn record_login_failure(&mut self) {
        self.consecutive_login_failures += 1;
        if self.consecutive_login_failures >= OTP_FAILURE_THRESHOLD {
            self.cooldown_until = Some(Instant::now() + OTP_COOLDOWN);
            warn!(
                "[{}] {} consecutive failures — entering {}s cooldown",
                self.host,
                self.consecutive_login_failures,
                OTP_COOLDOWN.as_secs()
            );
        }
    }

    /// Reset cooldown and back-off on an explicit user toggle.
    pub fn reset_circuit_breakers(&mut self) {
        self.consecutive_login_failures = 0;
        self.cooldown_until = None;
        self.probe_backoff_until = None;
        self.flap_count = 0;
        self.flap_backoff_until = None;
    }

    /// Return the ControlPath for a pool slot.
    pub fn pool_path(&self, index: usize) -> PathBuf {
        control::control_path(&self.host, index)
    }

    /// Rotate the active symlink to `index` if the other slot is Ready.
    /// Returns true if rotation happened.
    pub fn try_rotate(&mut self) -> bool {
        let now = Instant::now();
        let other = (self.active_index + 1) % POOL_SIZE;

        // Rotation is a RECOVERY action: flip away from a DOWN active slot onto a
        // Ready spare. If the active slot is itself Ready, there is nothing to
        // recover — rotating between two Ready slots just flip-flops the active
        // symlink pointlessly. Worse, the ping-pong guard below then re-arms
        // `probe_backoff` every rotation tick, and the dead-slot Restart/Rotate
        // paths skip while `in_probe_backoff()` — so a healthy pool's pointless
        // rotation also SUPPRESSES legitimate restarts. A healthy active slot
        // must never rotate. (This was the "rotation ping-pong detected" churn
        // logged every ~7s on connected hosts.)
        if self.slot_status[self.active_index] == SlotStatus::Ready {
            return false;
        }

        // Respect an armed probe/rotation backoff (the guard sets it but nothing
        // here honored it before, so the "backing off 60s" never took effect).
        if self.in_probe_backoff() {
            return false;
        }

        if self.slot_status[other] != SlotStatus::Ready {
            return false;
        }

        // Detect ping-pong
        if let Some(last) = self.last_rotate {
            if now.duration_since(last) < ROTATION_PING_PONG_WINDOW {
                self.probe_backoff_until = Some(now + PROBE_BACKOFF);
                warn!(
                    "[{}] rotation ping-pong detected; backing off {}s",
                    self.host,
                    PROBE_BACKOFF.as_secs()
                );
                return false;
            }
        }

        if control::update_symlink(&self.host, other) {
            self.active_index = other;
            self.last_rotate = Some(now);
            info!("[{}] rotated to pool slot {other}", self.host);
            return true;
        }
        false
    }

    /// Check if the active master's local socket is alive.
    pub fn active_master_ready(&self) -> bool {
        let path = self.pool_path(self.active_index);
        control::master_check(&path, &self.host)
    }
}

/// Map a probe result + current failure count to the legacy `Option<bool>`
/// "check_alive" that `next_action` consumes — applying hysteresis so a Ready
/// slot is only reported dead (`Some(false)`) after `threshold` consecutive
/// confident `Dead` probes. `Alive` → `Some(true)`; everything inconclusive or
/// below threshold → `None` (which `next_action` treats as "no restart").
pub fn probe_to_check(
    liveness: crate::ssh::control::MasterLiveness,
    consecutive_failures: u32,
    threshold: u32,
) -> Option<bool> {
    use crate::ssh::control::MasterLiveness::*;
    match liveness {
        Alive => Some(true),
        Inconclusive => None,
        Dead => {
            if consecutive_failures >= threshold {
                Some(false)
            } else {
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// start_master
// ---------------------------------------------------------------------------

/// Build the ssh argv and call `run_login`, then update `state`.
///
/// The `otp_provider` closure must already be guarded against OTP replay
/// (the engine layer serializes OTP submission per secret group, mirroring
/// Python's `_get_otp_group_lock` + `_fresh_otp_or_wait`).
///
/// Returns `true` iff the master is now Ready.
pub fn start_master(
    state: &mut PoolState,
    index: usize,
    password: &str,
    otp_provider: impl Fn() -> Result<String>,
) -> bool {
    if state.in_cooldown() {
        let secs = state
            .cooldown_until
            .map(|t| t.saturating_duration_since(Instant::now()).as_secs())
            .unwrap_or(0);
        info!(
            "[{}] skipping start_master({index}) — in cooldown ({secs}s left)",
            state.host
        );
        return false;
    }

    let path = state.pool_path(index);

    // Clean up any stale socket for this slot
    control::cleanup_stale_socket(&path, &state.host);

    // Build argv — mirrors _start_master_impl in backend.py exactly:
    //
    //   ssh -v -E <log> \
    //       -o StrictHostKeyChecking=no \
    //       -o UserKnownHostsFile=/dev/null \
    //       -o ServerAliveInterval=15 \
    //       -o ServerAliveCountMax=12 \
    //       -o ConnectTimeout=10 \
    //       -o ControlMaster=auto \
    //       -o ControlPath=<path> \
    //       -o ControlPersist=yes \
    //       <host>
    //
    // Note: -E sends ssh's own diagnostics to a per-slot file under /tmp for
    // debuggability; it does not affect connection semantics. We deliberately
    // do NOT pass -v: at verbose level a long-lived ControlPersist master
    // streams keepalive/debug spam into this append-only file forever (observed
    // at 8+ MB per slot), and nothing rotates it — a slow-burn /tmp (boot
    // volume) fill that can wedge the whole machine. Default verbosity keeps the
    // file tiny. Truncate any pre-existing (possibly huge, -v-era) file first.
    //
    // Pre-create with mode 0600 (and chmod an existing file down): the path is
    // fixed/predictable in shared /tmp, so on a multi-user machine the ssh
    // diagnostics (connection metadata — never the password/OTP, which go over
    // the pty) should not be world-readable. ssh -E appends to the existing
    // file, inheriting these permissions.
    let log_file = format!("/tmp/ssh2fa_ssh_master_{}_{index}.log", state.host);
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&log_file);
        // mode() only applies at creation — tighten a pre-existing 0644 file.
        let _ = std::fs::set_permissions(&log_file, std::fs::Permissions::from_mode(0o600));
    }
    let control_path_str = path.to_string_lossy().into_owned();

    let argv: Vec<String> = vec![
        "-E".into(),      log_file,
        "-o".into(),      "StrictHostKeyChecking=no".into(),
        "-o".into(),      "UserKnownHostsFile=/dev/null".into(),
        "-o".into(),      "ServerAliveInterval=15".into(),
        "-o".into(),      "ServerAliveCountMax=12".into(),
        "-o".into(),      "ConnectTimeout=10".into(),
        "-o".into(),      "ControlMaster=auto".into(),
        "-o".into(),      format!("ControlPath={control_path_str}"),
        "-o".into(),      "ControlPersist=yes".into(),
        state.host.clone(),
    ];

    info!("[{}] spawning ssh master slot {index}", state.host);

    match run_login(&argv, password, otp_provider) {
        Ok(LoginOutcome::Success) => {
            state.slot_status[index] = SlotStatus::Ready;
            state.consecutive_login_failures = 0;
            state.cooldown_until = None;
            // Start the uptime clock for flap detection (connect-then-drop).
            state.mark_slot_ready(index);
            info!("[{}] master slot {index} Ready", state.host);
            true
        }
        Ok(LoginOutcome::AuthFailed { reason }) => {
            warn!("[{}] master slot {index} auth failed: {reason}", state.host);
            state.slot_status[index] = SlotStatus::Failed;
            state.record_login_failure();
            false
        }
        Ok(LoginOutcome::Timeout) => {
            // A perpetually-timing-out host must trip the breaker too —
            // otherwise the heartbeat re-spawns a 60s login worker every cycle
            // forever. Mirror the AuthFailed/Err arms.
            warn!("[{}] master slot {index} login timed out", state.host);
            state.slot_status[index] = SlotStatus::Failed;
            state.record_login_failure();
            false
        }
        Ok(LoginOutcome::Eof { output }) => {
            // Early EOF (ssh died before the prompt) is also a failed attempt;
            // a host that keeps EOF-ing should likewise trip the breaker rather
            // than be re-driven every cycle.
            let reason = failure_reason(&output);
            warn!(
                "[{}] master slot {index} exited early: {reason}",
                state.host
            );
            state.slot_status[index] = SlotStatus::Dead;
            state.record_login_failure();
            false
        }
        Err(e) => {
            // A totp/system error (e.g. permanently-bad secret, OTP provider
            // failure) is treated like an auth failure for the circuit breaker:
            // otherwise the heartbeat would re-drive a hopeless login every
            // cycle forever. Mirror the AuthFailed arm's counter + cooldown.
            warn!("[{}] master slot {index} system error: {e}", state.host);
            state.slot_status[index] = SlotStatus::Dead;
            state.record_login_failure();
            false
        }
    }
}

/// Adopt an already-live ControlMaster instead of re-authenticating.
///
/// Probes every pool slot with `ssh -O check`. For each slot that responds,
/// marks it `Ready`. If any slot is alive, sets `active_index` to the slot the
/// active symlink already points at (when that one is alive) or the first alive
/// slot otherwise, and returns `true`.
///
/// This is what makes a daemon restart — or the Python→Rust cutover — a
/// **zero-relogin handoff**: a freshly started daemon finds the sockets the
/// previous owner left behind (same `ControlPath`, thanks to `ssh -G`
/// resolution) and takes them over without triggering 2FA. Returns `false` if
/// no slot is alive, in which case the caller should start a master normally.
pub fn adopt_if_alive(state: &mut PoolState) -> bool {
    let mut alive = [false; POOL_SIZE];
    let mut any = false;
    for (i, slot_alive) in alive.iter_mut().enumerate() {
        let path = control::control_path(&state.host, i);
        if control::master_check(&path, &state.host) {
            *slot_alive = true;
            any = true;
            state.slot_status[i] = SlotStatus::Ready;
            info!("[{}] adopted live master slot {i} (no login)", state.host);
        }
    }
    if !any {
        return false;
    }
    let preferred = control::symlink_target_index(&state.host)
        .filter(|&i| i < POOL_SIZE && alive[i]);
    state.active_index = preferred
        .unwrap_or_else(|| alive.iter().position(|&a| a).expect("any==true"));
    true
}

/// Tear down a specific pool slot: send `ssh -O exit`, clear status.
pub fn stop_slot(state: &mut PoolState, index: usize) {
    let path = state.pool_path(index);
    control::master_exit(&path, &state.host);
    control::cleanup_stale_socket(&path, &state.host);
    state.slot_status[index] = SlotStatus::Init;
}

/// Tear down all pool slots and remove the active symlink.
pub fn stop_all(state: &mut PoolState) {
    for i in 0..POOL_SIZE {
        stop_slot(state, i);
    }
    control::remove_symlink(&state.host);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_to_check_maps_with_hysteresis() {
        use crate::ssh::control::MasterLiveness::*;
        // Alive → always Some(true)
        assert_eq!(probe_to_check(Alive, 0, 3), Some(true));
        assert_eq!(probe_to_check(Alive, 9, 3), Some(true));
        // Inconclusive → never an answer
        assert_eq!(probe_to_check(Inconclusive, 5, 3), None);
        // Dead → None until the failure count reaches the threshold
        assert_eq!(probe_to_check(Dead, 1, 3), None);
        assert_eq!(probe_to_check(Dead, 2, 3), None);
        assert_eq!(probe_to_check(Dead, 3, 3), Some(false));
        assert_eq!(probe_to_check(Dead, 4, 3), Some(false));
    }

    #[test]
    fn note_probe_result_counts_only_confident_deaths() {
        use crate::ssh::control::MasterLiveness::*;
        let mut p = PoolState::new("k6");
        p.note_probe_result(0, Dead);
        p.note_probe_result(0, Dead);
        assert_eq!(p.consecutive_probe_failures[0], 2);
        // Inconclusive does not move the counter.
        p.note_probe_result(0, Inconclusive);
        assert_eq!(p.consecutive_probe_failures[0], 2);
        // A single Alive resets it.
        p.note_probe_result(0, Alive);
        assert_eq!(p.consecutive_probe_failures[0], 0);
        // Slots are independent.
        p.note_probe_result(1, Dead);
        assert_eq!(p.consecutive_probe_failures[1], 1);
        assert_eq!(p.consecutive_probe_failures[0], 0);
    }

    #[test]
    fn login_failure_increments_and_trips_cooldown_at_threshold() {
        // Mirrors the breaker logic that BOTH the AuthFailed and Err (totp/
        // system error) arms of start_master now run via record_login_failure:
        // a permanently-bad secret keeps bumping the counter and must arm the
        // cooldown once it crosses OTP_FAILURE_THRESHOLD.
        let mut pool = PoolState::new("auto2fa-unittest-host");
        assert_eq!(pool.consecutive_login_failures, 0);
        assert!(!pool.in_cooldown());

        // Below threshold: counter rises, no cooldown yet.
        for i in 1..OTP_FAILURE_THRESHOLD {
            pool.record_login_failure();
            assert_eq!(pool.consecutive_login_failures, i);
            assert!(!pool.in_cooldown(), "must not cool down before threshold");
            assert!(pool.cooldown_until.is_none());
        }

        // Crossing the threshold arms the cooldown.
        pool.record_login_failure();
        assert_eq!(pool.consecutive_login_failures, OTP_FAILURE_THRESHOLD);
        assert!(pool.cooldown_until.is_some(), "cooldown must be armed at threshold");
        assert!(pool.in_cooldown(), "must be in cooldown at threshold");
    }

    #[test]
    fn reset_circuit_breakers_clears_login_failures_and_cooldown() {
        let mut pool = PoolState::new("auto2fa-unittest-host");
        for _ in 0..OTP_FAILURE_THRESHOLD {
            pool.record_login_failure();
        }
        assert!(pool.in_cooldown());

        pool.reset_circuit_breakers();
        assert_eq!(pool.consecutive_login_failures, 0);
        assert!(pool.cooldown_until.is_none());
        assert!(!pool.in_cooldown());
    }

    #[test]
    fn try_rotate_noops_when_active_slot_ready() {
        // The live "rotation ping-pong" bug: a healthy active slot must NOT
        // rotate. Rotating between two Ready slots flip-flops the active symlink
        // pointlessly AND re-arms probe_backoff (which then suppressed real
        // restarts). A healthy pool must be a no-op with no backoff armed.
        let mut pool = PoolState::new("auto2fa-unittest-rotate-ready");
        pool.slot_status = [SlotStatus::Ready, SlotStatus::Ready];
        pool.active_index = 0;
        for _ in 0..5 {
            assert!(!pool.try_rotate(), "must not rotate a healthy active slot");
        }
        assert_eq!(pool.active_index, 0, "active slot must not flip-flop");
        assert!(
            pool.probe_backoff_until.is_none(),
            "a healthy pool must never arm the ping-pong backoff"
        );
    }

    #[test]
    fn try_rotate_respects_probe_backoff() {
        // Even with a down active slot + Ready spare, an armed probe backoff must
        // suppress rotation (previously the backoff was written but never read).
        let mut pool = PoolState::new("auto2fa-unittest-rotate-backoff");
        pool.slot_status = [SlotStatus::Dead, SlotStatus::Ready];
        pool.active_index = 0;
        pool.probe_backoff_until = Some(Instant::now() + Duration::from_secs(60));
        assert!(!pool.try_rotate(), "must not rotate while in probe backoff");
        assert_eq!(pool.active_index, 0);
    }

    #[test]
    fn flapping_connection_arms_backoff() {
        // Connect-then-immediate-drop FLAP_THRESHOLD times → arm the flap backoff
        // (the connect-then-drop case the login-failure breaker never caught).
        let mut pool = PoolState::new("auto2fa-unittest-flap");
        assert!(!pool.in_flap_backoff());
        for i in 1..=FLAP_THRESHOLD {
            pool.mark_slot_ready(0);
            pool.note_slot_dropped(0); // uptime ~0 < FLAP_MIN_UPTIME → flap
            assert_eq!(pool.flap_count, i);
        }
        assert!(pool.in_flap_backoff(), "must back off after {FLAP_THRESHOLD} flaps");
        // A user toggle (reset) clears it.
        pool.reset_circuit_breakers();
        assert_eq!(pool.flap_count, 0);
        assert!(!pool.in_flap_backoff());
    }

    #[test]
    fn drop_without_ready_marker_is_not_a_flap() {
        // A slot that was never marked Ready (no uptime clock) dropping must not
        // count as a flap.
        let mut pool = PoolState::new("auto2fa-unittest-noflap");
        pool.note_slot_dropped(0);
        assert_eq!(pool.flap_count, 0);
        assert!(!pool.in_flap_backoff());
    }

    #[test]
    fn long_lived_drop_resets_flaps() {
        // A connection that stayed up well past FLAP_MIN_UPTIME then died is NOT a
        // flap — it resets the counter. (Guarded so it's robust on a just-booted
        // host where Instant can't go back far enough.)
        if let Some(past) = Instant::now().checked_sub(FLAP_MIN_UPTIME + Duration::from_secs(5)) {
            let mut pool = PoolState::new("auto2fa-unittest-stable-drop");
            pool.flap_count = 2;
            pool.slot_ready_since[0] = Some(past);
            pool.note_slot_dropped(0);
            assert_eq!(pool.flap_count, 0, "a long-lived connection's death is not a flap");
        }
    }

    #[test]
    fn adopt_if_alive_false_when_no_master() {
        // A synthetic host with no ssh config and no live socket: every
        // `ssh -O check` fails, so adoption returns false and no slot is Ready.
        let mut pool = PoolState::new("auto2fa-unittest-no-such-host-xyz");
        assert!(!adopt_if_alive(&mut pool));
        assert!(pool
            .slot_status
            .iter()
            .all(|s| *s != SlotStatus::Ready));
    }
}
