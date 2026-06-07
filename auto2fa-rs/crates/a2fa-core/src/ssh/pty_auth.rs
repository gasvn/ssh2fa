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
use std::os::unix::io::RawFd;
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use portable_pty::{Child, CommandBuilder, NativePtySystem, PtySize, PtySystem};
use regex::Regex;

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// RAII child reaper
// ---------------------------------------------------------------------------

/// Owns the spawned ssh child for the duration of `run_login` and reaps it on
/// every FAILURE/abort exit path — normal return, `?`-operator early return, or
/// panic — while DETACHING (leaving it running) on the SUCCESS path.
///
/// portable-pty 0.8.1's `Child` is backed by `std::process::Child`, which has
/// NO Drop-reap, and the daemon installs no SIGCHLD handler. Without this guard
/// any failure path that dropped the child without `wait()` left a zombie
/// process and leaked the pty fd, accumulating on every attempt.
///
/// Two drop modes:
///
/// * SUCCESS (`detach_on_drop == true`, set by [`mark_success`]) — Drop is a
///   NO-OP: it neither kills nor waits. The ssh we spawn uses an INTERACTIVE
///   argv (no remote command) with `ControlMaster=auto` + `ControlPersist=yes`;
///   on success this foreground client IS the live, master-bearing pool client
///   and MUST stay alive (exactly like the pexpect child that `backend.py`
///   keeps in its pool). It is long-lived, NOT exiting, so a `wait()` here
///   would BLOCK FOREVER — wedging the (host,slot) and leaking a thread/child/
///   pty on every successful login. Detaching restores the working
///   pre-3135bcd behavior: just return and let the pty locals drop while the
///   client keeps running as the ControlPersist master for the pool.
///
/// * FAILURE (default, `detach_on_drop == false`) — `kill()` then `wait()`.
///   Used on every failure/abort path (timeout, auth-failed, eof, otp/write
///   `?` errors, system error) where the client is still blocked at a prompt
///   and must be force-terminated and reaped (no zombie / leaked pty fd).
struct ChildReaper {
    child: Box<dyn Child + Send + Sync>,
    detach_on_drop: bool,
}

impl ChildReaper {
    fn new(child: Box<dyn Child + Send + Sync>) -> Self {
        // Default to kill-and-reap so any early return / `?` / panic that
        // happens before we explicitly mark success force-terminates the
        // still-blocked client and reaps it.
        Self { child, detach_on_drop: false }
    }

    /// Mark the success path: Drop becomes a no-op (neither kill nor wait), so
    /// the live ControlPersist master-bearing client is left running for the
    /// pool. Waiting here would block forever (the client does not exit on a
    /// successful interactive login); killing it would tear down the master.
    fn mark_success(&mut self) {
        self.detach_on_drop = true;
    }
}

impl Drop for ChildReaper {
    fn drop(&mut self) {
        if self.detach_on_drop {
            // Success: leave the live master-bearing client running. No kill
            // (would tear down the master), no wait (would block forever).
            return;
        }
        // Failure/abort: force-terminate the still-blocked client and reap it
        // so no zombie / leaked pty fd remains. Best-effort.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

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

    // Spawn ssh in the slave side of the pty, then immediately wrap it in the
    // RAII reaper so EVERY subsequent exit path (including the `?`-operator
    // early returns just below and any panic) kills-and-reaps the child. The
    // `pair` itself (and thus the master/slave pty fds) is a local that drops
    // at function end on every path, so no fd outlives the function.
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| Error::Internal(format!("spawn ssh: {e}")))?;
    let mut reaper = ChildReaper::new(child);

    // Set the pty master to non-blocking BEFORE cloning the reader/writer.
    //
    // portable_pty 0.8.1 clones the reader and writer with `dup()`, so they
    // share the SAME open-file-description (and therefore its O_NONBLOCK status
    // flag) as the master fd we set here. Making the read non-blocking is what
    // lets us enforce LOGIN_TIMEOUT *during* the read instead of only at the
    // loop top: a `read()` with no data ready now returns `WouldBlock`
    // immediately, the loop sleeps a slice and re-checks the wall-clock
    // deadline, so a mid-login stall can never outrun the timeout. This keeps
    // the whole expect loop single-threaded — matching the working pre-3135bcd
    // structure — so the SUCCESS/detach path leaks no helper thread (the
    // live master-bearing client just keeps running and we return).
    //
    // The tiny password/OTP lines we write are far smaller than the pty buffer,
    // so a non-blocking write never short-writes in practice; `write_all` would
    // surface a `WouldBlock` as an error on the (failure) path if it ever did.
    if let Some(fd) = pair.master.as_raw_fd() {
        set_nonblocking(fd);
    }

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
        // Hard wall-clock deadline, checked at the loop top. Because the read
        // below is non-blocking, every WouldBlock returns here promptly, so a
        // mid-login stall can NEVER outrun LOGIN_TIMEOUT.
        if start.elapsed() >= LOGIN_TIMEOUT {
            warn!("ssh login timed out after {}s", LOGIN_TIMEOUT.as_secs());
            // reaper kills-and-reaps the still-blocked client on drop.
            return Ok(LoginOutcome::Timeout);
        }

        // Check if child has already exited
        match reaper.child.try_wait() {
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

        // Non-blocking read (O_NONBLOCK was set on the master fd). No data
        // ready → WouldBlock → sleep a slice and loop back to re-check the
        // wall-clock deadline.
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
                // Nothing yet — spin briefly, then re-check the deadline.
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {
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
            // Success: this interactive ssh client IS the live, master-bearing
            // ControlPersist pool client and must stay alive (like the pexpect
            // child backend.py keeps in its pool). mark_success() makes the
            // reaper DETACH on drop — no kill (would tear down the master), no
            // wait (would block forever; the client does not exit). We just
            // return and let the pty locals drop.
            reaper.mark_success();
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

/// Set `O_NONBLOCK` on a raw fd (best-effort). Used on the pty master so the
/// expect-loop read can honor the LOGIN_TIMEOUT deadline mid-read instead of
/// blocking forever. Failure is logged and ignored: the read then stays
/// blocking, which is strictly no worse than the pre-fix behavior.
fn set_nonblocking(fd: RawFd) {
    // SAFETY: fd is a valid open pty master fd owned by `pair.master` for the
    // duration of this call; F_GETFL/F_SETFL on it are standard, side-effect
    // -free flag manipulation.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            warn!("fcntl(F_GETFL) on pty master failed; read stays blocking");
            return;
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            warn!("fcntl(F_SETFL, O_NONBLOCK) on pty master failed; read stays blocking");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — only the RAII reaper Drop semantics (everything else needs a real
// ssh + pty + TOTP, which is impractical to unit-test; see the module note).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use portable_pty::{ChildKiller, ExitStatus};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A fake `Child` whose `kill`/`wait` bump shared counters so a test can
    /// assert what the reaper's `Drop` did after the reaper is dropped.
    #[derive(Debug)]
    struct FakeChild {
        kills: Arc<AtomicUsize>,
        waits: Arc<AtomicUsize>,
    }

    impl ChildKiller for FakeChild {
        fn kill(&mut self) -> std::io::Result<()> {
            self.kills.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            // Not exercised by the reaper; return an independent no-op-ish killer.
            Box::new(FakeChild {
                kills: self.kills.clone(),
                waits: self.waits.clone(),
            })
        }
    }

    impl Child for FakeChild {
        fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
            Ok(None)
        }
        fn wait(&mut self) -> std::io::Result<ExitStatus> {
            self.waits.fetch_add(1, Ordering::SeqCst);
            Ok(ExitStatus::with_exit_code(0))
        }
        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    fn fake() -> (ChildReaper, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let kills = Arc::new(AtomicUsize::new(0));
        let waits = Arc::new(AtomicUsize::new(0));
        let child = Box::new(FakeChild {
            kills: kills.clone(),
            waits: waits.clone(),
        });
        (ChildReaper::new(child), kills, waits)
    }

    #[test]
    fn drop_default_kills_then_waits_exactly_once() {
        // Failure/abort path: detach_on_drop stays false → kill() + wait().
        let (reaper, kills, waits) = fake();
        drop(reaper);
        assert_eq!(kills.load(Ordering::SeqCst), 1, "must kill once on failure path");
        assert_eq!(waits.load(Ordering::SeqCst), 1, "must reap (wait) exactly once");
    }

    #[test]
    fn drop_after_mark_success_detaches_neither_kills_nor_waits() {
        // Success path: mark_success() → DETACH. Drop must NOT kill (would tear
        // down the ControlPersist master) and must NOT wait (the live
        // master-bearing client never exits → wait() would block forever).
        let (mut reaper, kills, waits) = fake();
        reaper.mark_success();
        drop(reaper);
        assert_eq!(kills.load(Ordering::SeqCst), 0, "success path must NOT kill the live master client");
        assert_eq!(waits.load(Ordering::SeqCst), 0, "success path must NOT wait (would block forever)");
    }
}
