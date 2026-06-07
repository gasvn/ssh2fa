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

    /// Reset cooldown and back-off on an explicit user toggle.
    pub fn reset_circuit_breakers(&mut self) {
        self.consecutive_login_failures = 0;
        self.cooldown_until = None;
        self.probe_backoff_until = None;
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
    // Note: -v and -E are kept for debuggability (written to /tmp); they do
    // not affect connection semantics.
    let log_file = format!("/tmp/auto2fa_ssh_master_{}_{index}.log", state.host);
    let control_path_str = path.to_string_lossy().into_owned();

    let argv: Vec<String> = vec![
        "-v".into(),
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
            info!("[{}] master slot {index} Ready", state.host);
            true
        }
        Ok(LoginOutcome::AuthFailed { reason }) => {
            warn!("[{}] master slot {index} auth failed: {reason}", state.host);
            state.slot_status[index] = SlotStatus::Failed;
            state.consecutive_login_failures += 1;
            if state.consecutive_login_failures >= OTP_FAILURE_THRESHOLD {
                state.cooldown_until = Some(Instant::now() + OTP_COOLDOWN);
                warn!(
                    "[{}] {} consecutive failures — entering {}s cooldown",
                    state.host,
                    state.consecutive_login_failures,
                    OTP_COOLDOWN.as_secs()
                );
            }
            false
        }
        Ok(LoginOutcome::Timeout) => {
            warn!("[{}] master slot {index} login timed out", state.host);
            state.slot_status[index] = SlotStatus::Failed;
            false
        }
        Ok(LoginOutcome::Eof { output }) => {
            let reason = failure_reason(&output);
            warn!(
                "[{}] master slot {index} exited early: {reason}",
                state.host
            );
            state.slot_status[index] = SlotStatus::Dead;
            false
        }
        Err(e) => {
            warn!("[{}] master slot {index} system error: {e}", state.host);
            state.slot_status[index] = SlotStatus::Dead;
            false
        }
    }
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
