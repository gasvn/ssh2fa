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

/// Owns the spawned ssh child for the duration of `run_login`.
///
/// portable-pty 0.8.1's `Child` is backed by `std::process::Child`, which has
/// NO Drop-reap, and the daemon installs no SIGCHLD handler. Without this guard
/// any failure path that dropped the child without `wait()` left a zombie
/// process and leaked the pty fd, accumulating on every attempt.
///
/// The child lives in an `Option`:
///
/// * FAILURE/abort path (the child is still owned here) — Drop `kill()`s the
///   client, then reaps it on a DETACHED, BOUNDED thread. Used on every
///   failure/abort path (timeout, auth-failed, eof, otp/write `?` errors,
///   panic, system error). The reap is detached because a synchronous
///   `wait()` in Drop blocked FOREVER for ssh children that ignore SIGKILL
///   while stuck in an uninterruptible syscall — and Drop runs on the per-host
///   login worker thread, so a blocked wait() wedged the worker, which then
///   never released its `StartGuard` token → the heartbeat saw "restart already
///   in flight" forever and reconnection starved process-wide (observed: ~14
///   workers stuck in this Drop, the daemon stopped maintaining EVERY
///   connection). Detaching frees the worker; bounding keeps the detached
///   reaper from leaking forever if the child is truly un-reapable.
///
/// * SUCCESS path — the caller pulls the child OUT with [`take_child`] and
///   reaps it on a detached thread AFTER dropping the pty fds (see `run_login`).
///   Once taken, Drop is a NO-OP (`Option` is `None`), so the reaper neither
///   kills nor blocks. The ssh we spawn uses an INTERACTIVE argv (no remote
///   command) with `ControlMaster=auto` + `ControlPersist=yes`; on success this
///   foreground client backgrounds the persistent master (triggered by the pty
///   master fd closing → SIGHUP) and then EXITS. We must NOT kill it (that
///   would tear down the persistent master), and we must NOT block `run_login`
///   waiting for it. The detached `wait()` reaps the now-exiting foreground
///   client (so no zombie) without blocking the caller; the persistent master
///   stays alive for the pool.
struct ChildReaper {
    child: Option<Box<dyn Child + Send + Sync>>,
}

impl ChildReaper {
    fn new(child: Box<dyn Child + Send + Sync>) -> Self {
        // Owns the child; Drop kill-and-reaps it unless `take_child` pulls it
        // out first (the success path).
        Self { child: Some(child) }
    }

    /// SUCCESS path: take ownership of the child out of the reaper so it can be
    /// reaped on a detached thread by the caller (after the pty fds are
    /// dropped). After this, the reaper's Drop is a no-op. Returns `None` only
    /// if already taken (never happens in practice).
    fn take_child(&mut self) -> Option<Box<dyn Child + Send + Sync>> {
        self.child.take()
    }
}

impl Drop for ChildReaper {
    fn drop(&mut self) {
        // Only the FAILURE/abort path still owns the child here. The success
        // path took the child via `take_child`, leaving `None` → no-op.
        if let Some(mut child) = self.child.take() {
            // Force-terminate the still-blocked client, then reap on a DETACHED,
            // BOUNDED thread. NEVER block here: a synchronous `child.wait()` in
            // Drop wedged the login worker (and starved reconnection) whenever a
            // killed ssh child ignored SIGKILL in an uninterruptible syscall.
            let _ = child.kill();
            let spawned = std::thread::Builder::new()
                .name("login-reap-fail".into())
                .spawn(move || {
                    // Poll for the killed child up to a deadline, then give up
                    // (the OS reaps the zombie when the daemon exits — a bounded
                    // leak, never a wedge).
                    let deadline = Instant::now() + Duration::from_secs(30);
                    loop {
                        match child.try_wait() {
                            Ok(Some(_)) => break,           // reaped
                            Ok(None) => {
                                if Instant::now() >= deadline {
                                    let _ = child.kill();   // last-ditch
                                    break;
                                }
                                std::thread::sleep(Duration::from_millis(100));
                            }
                            Err(_) => break,
                        }
                    }
                });
            // If even the reaper thread can't spawn (EAGAIN under thread
            // pressure), do NOT fall back to a blocking wait() — that blocking
            // is exactly the wedge we are removing. Drop the (killed) child
            // unreaped; it becomes a transient zombie reaped at daemon exit.
            let _ = spawned;
        }
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

/// Marker emitted by `host_test_credentials`' remote command
/// (`echo __auto2fa_login_ok__`). In command mode there is NO shell prompt —
/// after auth the remote prints this marker and ssh exits — so the expect loop
/// must accept it as a success signal. Without it, CORRECT credentials were
/// classified `Eof` and reported to the user as "host unreachable" (the Python
/// reference matched this marker as its success pattern, daemon.py).
pub const LOGIN_OK_MARKER: &str = "__auto2fa_login_ok__";

/// Chunk size for pty reads.
const READ_BUF: usize = 4096;

/// Hard cap on the retained login transcript.
///
/// A misbehaving / malicious / merely chatty server can stream data into the
/// pty for up to `LOGIN_TIMEOUT`. Without a cap, `buf` would grow to hundreds of
/// MB (heap exhaustion across concurrent login workers) AND every loop iteration
/// re-runs the prompt regexes over the WHOLE buffer (O(n²) CPU) — the exact
/// memory+CPU exhaustion class that has hung this machine. All prompts
/// (Password:/Verification code:/shell `$`) appear at the TAIL, so retaining
/// only the trailing window keeps detection correct while bounding both.
const MAX_TRANSCRIPT: usize = 256 * 1024;

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
            // 2FA / OTP prompt. Beyond FAS-RC Duo's "Verification code:" this
            // covers the common keyboard-interactive variants so the tool isn't
            // Duo-only: passcode, Duo's "Passcode or option (1-1):" menu form,
            // one-time password/code, OTP, token, {2FA,MFA,authentication}
            // code. Each keyword is anchored to a trailing ':' (optionally via
            // an " or option (…)" clause) so a banner/MOTD line that merely
            // mentions a passcode can't false-match and make us send the code
            // into prose. OTP is matched BEFORE the password prompt in the
            // loop, and "Password:" contains none of these keywords, so the
            // two never collide.
            otp: Regex::new(
                r"(?i)(verification.?code|passcode|one.?time.?(?:password|passcode|code)|otp|token|2fa.?code|mfa.?code|authentication.?code|security.?code)(?:\s+or\s+option[^:]*)?:",
            )
            .map_err(|e| Error::Internal(e.to_string()))?,
            // A bare "$" or "#" — naive but matches what pexpect uses.
            // `(^|[^$#])` guard: a banner separator line like "#####" also ends
            // in `#\s*$` and used to be a FALSE login success before any
            // credential was sent. A real prompt's symbol is preceded by a
            // path/space/bracket (or starts the line for root's bare "# "),
            // never by another $/#.
            shell_prompt: Regex::new(r"(^|[^$#])[$#]\s*$")
                .map_err(|e| Error::Internal(e.to_string()))?,
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
    // Time spent INSIDE otp_provider() (the shared-secret replay wait can sleep
    // up to ~31 s per TOTP window) must not eat the login budget: Python slept
    // BEFORE sendline and then ran a fresh 60 s post-OTP expect. Without this,
    // the 2nd/3rd host sharing one Duo secret systematically timed out.
    let mut deadline_extension = Duration::ZERO;

    'outer: loop {
        // Hard wall-clock deadline, checked at the loop top. Because the read
        // below is non-blocking, every WouldBlock returns here promptly, so a
        // mid-login stall can NEVER outrun the deadline.
        if start.elapsed() >= LOGIN_TIMEOUT + deadline_extension {
            warn!("ssh login timed out after {}s", LOGIN_TIMEOUT.as_secs());
            // reaper kills-and-reaps the still-blocked client on drop.
            return Ok(LoginOutcome::Timeout);
        }

        // Check if child has already exited. `expect` is safe: the child is
        // only taken out of the reaper on the SUCCESS return path below, which
        // exits the loop immediately, so it is always present here.
        match reaper
            .child
            .as_mut()
            .expect("child present during expect loop")
            .try_wait()
        {
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
                cap_transcript(&mut buf, MAX_TRANSCRIPT);
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

        // Success: shell prompt detected, or the test-credentials marker
        // (command mode prints the marker instead of a prompt).
        if pat.shell_prompt.is_match(&buf) || buf.contains(LOGIN_OK_MARKER) {
            info!("ssh login successful (shell prompt detected)");
            // On success this interactive ssh client (ControlMaster=auto +
            // ControlPersist=yes) backgrounds the persistent master and then
            // EXITS as a foreground process. We must:
            //   (a) NOT kill it — that would tear down the persistent master;
            //   (b) NOT block run_login waiting for it; and
            //   (c) NOT leave it a zombie — something must wait() it.
            //
            // Sequence (ordering matters):
            //   1. Pull the child OUT of the reaper so the reaper's Drop becomes
            //      a no-op (no kill, no blocking wait at function exit).
            //   2. Drop the pty handles (writer, reader, and the pty pair/master
            //      fd). Closing the master fd is what sends SIGHUP to the
            //      foreground client → it backgrounds the ControlPersist master
            //      (which keeps running, adoptable via master_check) and then
            //      the foreground process exits.
            //   3. Reap the now-exiting foreground client on a DETACHED thread:
            //      wait() returns quickly (the client is exiting), reaps it so
            //      no zombie remains, and never blocks run_login. The persistent
            //      master is untouched and stays alive for the pool.
            let child = reaper.take_child();
            // Explicitly drop the pty handles so the master fd closes → SIGHUP.
            drop(writer);
            drop(reader);
            drop(pair);
            if let Some(child) = child {
                // Detached reaper: never blocks run_login, never leaves a
                // zombie. Builder::spawn so a spawn-Err (EAGAIN) can't panic.
                // On spawn-Err we log a warn and skip the detached reap (we must
                // NOT block run_login). At worst this leaves a transient zombie
                // of an already-exiting foreground client, reaped by the OS when
                // the daemon eventually exits.
                let spawn_res = std::thread::Builder::new()
                    .name("login-reap".into())
                    .spawn(move || {
                        let mut child = child;
                        let _ = child.wait();
                    });
                if let Err(e) = spawn_res {
                    warn!("could not spawn login-reap thread ({e}); skipping detached reap");
                }
            }
            return Ok(LoginOutcome::Success);
        }

        // Failure after ANY credential was sent. These used to be gated on
        // otp_sent only, so a wrong password (server prints "Permission
        // denied"/re-prompts "Password:" BEFORE any OTP exchange) matched
        // nothing, burned the full 60 s, and was reported as "login timed
        // out" instead of an auth failure.
        if password_sent || otp_sent {
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
            // Server re-prompted for a password AFTER we already sent one
            // (buf was cleared after sending) — credential rejected. Post-OTP
            // this is the classic replay/loop-back; pre-OTP it means the
            // password itself was wrong.
            if pat.password.is_match(&buf) {
                return Ok(LoginOutcome::AuthFailed {
                    reason: if otp_sent {
                        "Server looped back to Password prompt".into()
                    } else {
                        "Password rejected (server re-prompted)".into()
                    },
                });
            }
        }

        // OTP prompt (before or after password)
        if !otp_sent && pat.otp.is_match(&buf) {
            let otp_t0 = Instant::now();
            let code = otp_provider()?;
            // Replay-wait time doesn't count against the login budget.
            deadline_extension += otp_t0.elapsed();
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
        // One last check for success/failure patterns in the drained buffer.
        // The marker case matters here: in command mode ssh EXITS right after
        // printing the marker, so success is usually detected post-loop.
        if pat.shell_prompt.is_match(&buf) || buf.contains(LOGIN_OK_MARKER) {
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

/// Keep `buf` bounded by retaining only its trailing window once it exceeds
/// `max`. Prompts always appear at the tail, so dropping the head never breaks
/// detection. Drains to a char boundary so the `String` stays valid UTF-8.
fn cap_transcript(buf: &mut String, max: usize) {
    if buf.len() <= max {
        return;
    }
    // Target keeping the last max/2 bytes (leaves headroom before the next cap).
    let mut cut = buf.len() - max / 2;
    while cut < buf.len() && !buf.is_char_boundary(cut) {
        cut += 1;
    }
    buf.drain(..cut);
}

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

    /// A fake `Child` whose `kill`/`wait`/`try_wait` bump shared counters so a
    /// test can assert what the reaper's `Drop` did. `try_wait` reports the
    /// child as exited once it has been killed, so the failure-path detached
    /// reaper (which polls `try_wait`) reaps promptly instead of spinning.
    #[derive(Debug)]
    struct FakeChild {
        kills: Arc<AtomicUsize>,
        waits: Arc<AtomicUsize>,
        try_waits: Arc<AtomicUsize>,
        killed: bool,
    }

    impl ChildKiller for FakeChild {
        fn kill(&mut self) -> std::io::Result<()> {
            self.kills.fetch_add(1, Ordering::SeqCst);
            self.killed = true;
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(FakeChild {
                kills: self.kills.clone(),
                waits: self.waits.clone(),
                try_waits: self.try_waits.clone(),
                killed: self.killed,
            })
        }
    }

    impl Child for FakeChild {
        fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
            self.try_waits.fetch_add(1, Ordering::SeqCst);
            Ok(if self.killed { Some(ExitStatus::with_exit_code(0)) } else { None })
        }
        fn wait(&mut self) -> std::io::Result<ExitStatus> {
            self.waits.fetch_add(1, Ordering::SeqCst);
            Ok(ExitStatus::with_exit_code(0))
        }
        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    fn fake() -> (ChildReaper, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let kills = Arc::new(AtomicUsize::new(0));
        let waits = Arc::new(AtomicUsize::new(0));
        let try_waits = Arc::new(AtomicUsize::new(0));
        let child = Box::new(FakeChild {
            kills: kills.clone(),
            waits: waits.clone(),
            try_waits: try_waits.clone(),
            killed: false,
        });
        (ChildReaper::new(child), kills, waits, try_waits)
    }

    #[test]
    fn cap_transcript_bounds_size_and_preserves_tail_prompt() {
        let max = 1024;
        let mut buf = String::new();
        // Simulate a server flooding the pty, then finally emitting the prompt.
        for _ in 0..10_000 {
            buf.push_str("noise noise noise noise ");
            cap_transcript(&mut buf, max);
            // Must stay bounded at every step (never balloon to MBs).
            assert!(buf.len() <= max, "transcript exceeded cap: {}", buf.len());
        }
        buf.push_str("Password:");
        cap_transcript(&mut buf, max);
        assert!(buf.len() <= max);
        // The tail prompt the expect loop matches on must survive.
        assert!(buf.ends_with("Password:"), "tail prompt must be retained: {buf:?}");
        let pat = Patterns::new().unwrap();
        assert!(pat.password.is_match(&buf), "password regex must still match after capping");
    }

    /// The OTP matcher must accept the common 2FA prompt variants (not just
    /// Duo's "Verification code:") while never matching a non-prompt line —
    /// and never stealing the password prompt.
    #[test]
    fn otp_prompt_variants() {
        let pat = Patterns::new().unwrap();
        for ok in [
            "Verification code: ",
            "VerificationCode:",
            "verification code:",
            "Token:",
            "Passcode: ",
            "Passcode or option (1-1): ",        // Duo menu form
            "OTP: ",
            "One-time password: ",
            "One-time code:",
            "2FA code: ",
            "MFA code:",
            "Authentication code: ",
            "Security code:",
        ] {
            assert!(pat.otp.is_match(ok), "OTP prompt must match: {ok:?}");
        }
        for no in [
            "Password: ",                                  // password, not OTP
            "Enter a passcode or select one of the following options:", // instructional line, not the prompt
            "Your duo passcode keeps you secure.",         // banner prose (no ':' after keyword)
            "Last login: Tue from 1.2.3.4",
            "Welcome to the cluster!",
        ] {
            assert!(!pat.otp.is_match(no), "must NOT match non-prompt: {no:?}");
        }
        // The password prompt must NOT be eaten by the OTP matcher.
        assert!(!pat.otp.is_match("Password:"));
        assert!(pat.password.is_match("Password:"));
    }

    #[test]
    fn cap_transcript_noop_when_under_cap() {
        let mut buf = "short transcript ending in Password:".to_string();
        let before = buf.clone();
        cap_transcript(&mut buf, 256 * 1024);
        assert_eq!(buf, before, "under-cap transcript must be untouched");
    }

    #[test]
    fn cap_transcript_keeps_valid_utf8_on_multibyte_boundary() {
        let max = 16;
        // Fill with a multibyte char so a naive byte cut would split it.
        let mut buf = "🔒".repeat(50); // 4 bytes each
        cap_transcript(&mut buf, max);
        assert!(buf.len() <= max);
        // If this is reachable, the String is valid UTF-8 (would have panicked otherwise).
        assert!(buf.chars().all(|c| c == '🔒'));
    }

    #[test]
    fn drop_failure_path_kills_then_reaps_detached_without_blocking() {
        // Failure/abort path: Drop kills synchronously, then reaps on a DETACHED
        // bounded thread — it must NOT block the dropping (worker) thread. The
        // old synchronous wait()-in-Drop wedged login workers and starved
        // reconnection.
        let (reaper, kills, _waits, try_waits) = fake();
        drop(reaper); // must return immediately (no blocking wait here)
        assert_eq!(kills.load(Ordering::SeqCst), 1, "must kill once on failure path");
        // The reap runs on a detached thread — poll briefly for it.
        let mut reaped = false;
        for _ in 0..200 {
            if try_waits.load(Ordering::SeqCst) >= 1 {
                reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(reaped, "detached reaper must try_wait()-reap the killed child");
    }

    #[test]
    fn take_child_makes_drop_a_noop_and_hands_child_to_caller() {
        // Success path: the caller pulls the child OUT of the reaper with
        // take_child() and reaps it on a detached thread (after dropping the
        // pty fds). The reaper itself must then NOT kill (would tear down the
        // ControlPersist master) and must NOT wait on drop. The detached reap
        // is what wait()s the exiting foreground client (no zombie).
        let (mut reaper, kills, waits, try_waits) = fake();
        let mut child = reaper
            .take_child()
            .expect("take_child yields the child on the success path");
        // Reaper dropped with the child already taken → no-op (no kill/wait).
        drop(reaper);
        assert_eq!(
            kills.load(Ordering::SeqCst),
            0,
            "success path must NOT kill the live master client"
        );
        assert_eq!(
            waits.load(Ordering::SeqCst),
            0,
            "reaper drop must NOT wait after the child was taken"
        );
        assert_eq!(
            try_waits.load(Ordering::SeqCst),
            0,
            "reaper drop with child taken must not reap at all"
        );
        // The caller's detached reaper waits the now-exiting foreground client
        // exactly once → reaped, not zombied. (Done inline here in the test.)
        let _ = child.wait();
        assert_eq!(
            waits.load(Ordering::SeqCst),
            1,
            "the taken child is reaped via the detached path (waited once)"
        );
        assert_eq!(
            kills.load(Ordering::SeqCst),
            0,
            "success path never kills the persistent master"
        );
    }
}
