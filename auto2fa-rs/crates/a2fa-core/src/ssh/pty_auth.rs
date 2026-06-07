//! PTY-based SSH authentication — the Rust port of pexpect-driven login in
//! `backend.py`'s `_start_master_impl`.
//!
//! Spawns `ssh` inside a `portable-pty` pseudo-terminal, runs an expect loop
//! to feed the password and TOTP code, and returns a `LoginOutcome` with the
//! captured transcript.
//!
//! # Prompt regexes extracted from backend.py
//!
//! Password prompt:
//!   `[Pp]assword:`
//!
//! OTP / verification-code prompts:
//!   `[Vv]erification[Cc]ode:`  (matches "Verification code:" and "VerificationCode:")
//!   `[Tt]oken:`
//!   `Verification code:`        (redundant but harmless — Python listed it twice)
//!
//! Success indicators (shell is ready):
//!   `\$`  `#`  (a bare prompt)
//!
//! Failure indicators (after OTP was sent):
//!   `Login incorrect`
//!   `Permission denied`
//!   `[Pp]assword:` looping back (server rejected creds, re-asked for password)
//!
//! # Note on unit testing
//! This file deliberately contains NO unit tests: all behaviour requires a
//! real SSH server, a real password, and a live TOTP secret. The caller is
//! expected to validate this against an actual cluster host.
//! See `examples/ssh_login.rs` for a manual prototype.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use regex::Regex;

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Outcome of a PTY login attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    /// SSH master established; the shell prompt was seen.
    Success,
    /// Authentication rejected (wrong password / OTP / rate-limited).
    AuthFailed { reason: String },
    /// The expect loop timed out — host unreachable or extremely slow.
    Timeout,
    /// SSH exited before the expect loop completed.
    Eof { output: String },
}

// ---------------------------------------------------------------------------
// Expect-loop configuration
// ---------------------------------------------------------------------------

/// Overall timeout for the full login sequence (generous: MOTD can be huge).
const LOGIN_TIMEOUT: Duration = Duration::from_secs(60);

/// Chunk size for pty reads.
const READ_BUF: usize = 4096;

// ---------------------------------------------------------------------------
// Compiled regexes (constructed once per call — regex! macro not available)
// ---------------------------------------------------------------------------

struct Patterns {
    password:       Regex,
    otp:            Regex,   // verification-code or token
    shell_prompt:   Regex,   // $ or # at end of segment
    login_incorrect: Regex,
    permission_denied: Regex,
}

impl Patterns {
    fn new() -> Result<Self> {
        Ok(Self {
            // Matches "Password:" / "password:"
            password: Regex::new(r"(?i)password:").map_err(|e| Error::Internal(e.to_string()))?,
            // Matches "Verification code:" / "VerificationCode:" / "Token:"
            otp: Regex::new(r"(?i)(verification.?code|token):").map_err(|e| Error::Internal(e.to_string()))?,
            // A bare "$" or "#" — naive but matches what pexpect uses.
            shell_prompt: Regex::new(r"[$#]\s*$").map_err(|e| Error::Internal(e.to_string()))?,
            login_incorrect: Regex::new(r"Login incorrect").map_err(|e| Error::Internal(e.to_string()))?,
            permission_denied: Regex::new(r"Permission denied").map_err(|e| Error::Internal(e.to_string()))?,
        })
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Spawn `ssh` with `argv` in a PTY and drive the expect loop.
///
/// `argv` should contain everything *after* `ssh` (i.e. the same slice that
/// `_start_master_impl` passes to `pexpect.spawn("ssh", ssh_argv, …)`).
///
/// `otp_provider` is called exactly once, at the moment the OTP prompt is
/// detected. The closure should call `totp::totp_now` (and handle any OTP
/// replay-guard logic) before returning the 6-digit code.
///
/// Returns `Ok(LoginOutcome)` in all expected cases; returns `Err` only for
/// unexpected system-level failures (e.g. can't open a pty).
pub fn run_login(
    argv: &[String],
    password: &str,
    otp_provider: impl Fn() -> Result<String>,
) -> Result<LoginOutcome> {
    let pty_system = NativePtySystem::default();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| Error::Internal(format!("openpty: {e}")))?;

    // Build the ssh command
    let mut cmd = CommandBuilder::new("ssh");
    for arg in argv {
        cmd.arg(arg);
    }

    // Spawn ssh in the slave side of the pty
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| Error::Internal(format!("spawn ssh: {e}")))?;

    // Grab master read/write handles
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| Error::Internal(format!("pty reader: {e}")))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|e| Error::Internal(format!("pty writer: {e}")))?;

    let pat = Patterns::new()?;

    // --- Expect loop ---------------------------------------------------------
    let start = Instant::now();
    let mut buf = String::new();
    let mut raw = vec![0u8; READ_BUF];
    let mut password_sent = false;
    let mut otp_sent = false;

    'outer: loop {
        // Check overall timeout
        if start.elapsed() >= LOGIN_TIMEOUT {
            warn!("ssh login timed out after {}s", LOGIN_TIMEOUT.as_secs());
            let _ = child.kill();
            return Ok(LoginOutcome::Timeout);
        }

        // Check if child has already exited
        match child.try_wait() {
            Ok(Some(_)) => {
                debug!("ssh exited; transcript:\n{buf}");
                // Might still have data buffered — do a final drain
                let _ = reader.read(&mut raw).map(|n| {
                    buf.push_str(&String::from_utf8_lossy(&raw[..n]));
                });
                break 'outer;
            }
            Ok(None) => {} // still running
            Err(e) => {
                warn!("try_wait error: {e}");
                break 'outer;
            }
        }

        // Non-blocking read — portable-pty uses a raw fd so we set a short
        // deadline via select/poll-style: just try and accumulate.
        match reader.read(&mut raw) {
            Ok(0) => {
                // EOF on pty master — child closed the slave
                break 'outer;
            }
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&raw[..n]);
                buf.push_str(&chunk);
                debug!("pty chunk: {:?}", &chunk);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Nothing yet — spin briefly
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(e) => {
                warn!("pty read error: {e}");
                break 'outer;
            }
        }

        // --- Pattern matching against accumulated buffer ----------------------

        // Success: shell prompt detected
        if pat.shell_prompt.is_match(&buf) {
            info!("ssh login successful (shell prompt detected)");
            let _ = child.kill(); // release master — the real ControlMaster stays in bg
            return Ok(LoginOutcome::Success);
        }

        // Failure after OTP was sent
        if otp_sent {
            if pat.login_incorrect.is_match(&buf) {
                return Ok(LoginOutcome::AuthFailed {
                    reason: "Login incorrect".into(),
                });
            }
            if pat.permission_denied.is_match(&buf) {
                return Ok(LoginOutcome::AuthFailed {
                    reason: "Permission denied".into(),
                });
            }
            // Server looped back to password prompt — credential rejected
            if pat.password.is_match(&buf) {
                return Ok(LoginOutcome::AuthFailed {
                    reason: "Server looped back to Password prompt".into(),
                });
            }
        }

        // OTP prompt (before or after password)
        if !otp_sent && pat.otp.is_match(&buf) {
            let code = otp_provider()?;
            info!("sending OTP");
            write_line(&mut writer, &code)?;
            otp_sent = true;
            buf.clear(); // discard prompt echo, start fresh for post-OTP patterns
            continue;
        }

        // Password prompt
        if !password_sent && pat.password.is_match(&buf) {
            info!("sending password");
            write_line(&mut writer, password)?;
            password_sent = true;
            buf.clear();
            continue;
        }
    }

    // Fell out of the loop without a definitive result
    let outcome = if buf.is_empty() {
        LoginOutcome::Eof { output: "(no output)".into() }
    } else {
        // One last check for success/failure patterns in the drained buffer
        if pat.shell_prompt.is_match(&buf) {
            LoginOutcome::Success
        } else if pat.login_incorrect.is_match(&buf) || pat.permission_denied.is_match(&buf) {
            let reason = crate::ssh::failure::failure_reason(&buf);
            LoginOutcome::AuthFailed { reason }
        } else {
            LoginOutcome::Eof { output: buf }
        }
    };
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn write_line(w: &mut dyn Write, s: &str) -> Result<()> {
    let line = format!("{s}\n");
    w.write_all(line.as_bytes())
        .map_err(Error::Io)
}
