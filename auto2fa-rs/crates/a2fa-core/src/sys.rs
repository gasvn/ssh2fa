//! Process-level system resource configuration.

use std::io::{ErrorKind, Read};
use std::os::unix::io::AsRawFd;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use log::{info, warn};

/// Drain whatever is already buffered on a child pipe WITHOUT waiting for EOF.
///
/// `read_to_end` blocks until the write-end closes. If the spawned child forks a
/// daemonized grandchild that INHERITS the pipe (sshfs→FUSE server, any
/// double-fork), the write-end stays open after the parent exits and a
/// `read_to_end` would block FOREVER — defeating the whole point of a "bounded"
/// runner. We only need what the parent wrote before exiting (small output), so
/// set the fd non-blocking and read until `WouldBlock`/EOF. A grandchild holding
/// the pipe just means we stop at the buffered data instead of hanging.
fn drain_nonblocking<R: AsRawFd + Read>(r: &mut R) -> Vec<u8> {
    // SAFETY: r owns a valid open pipe fd for the duration of this call;
    // F_GETFL/F_SETFL are side-effect-free flag ops.
    unsafe {
        let flags = libc::fcntl(r.as_raw_fd(), libc::F_GETFL);
        if flags >= 0 {
            let _ = libc::fcntl(r.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match r.read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => break, // nothing more buffered
            Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    out
}

/// Run an external command with a HARD timeout, killing+reaping it on deadline.
///
/// This is the generic sibling of `ssh::control::run_ssh_bounded` for non-ssh
/// helpers (pgrep/ps/kill, which/umount/sshfs). It enforces the closing
/// invariant for any blocking external spawn: a hard kill-on-deadline plus
/// `wait()`-reap on EVERY exit path, so a wedged child can never pin the caller
/// thread forever and never leaks a zombie/fd.
///
/// Returns `Some(Output)` if the child exits before `timeout`, `None` if it had
/// to be killed (deadline) or could not be spawned. stdin is `/dev/null`;
/// stdout/stderr are captured.
///
/// NOTE: output is read only AFTER the child exits, so this is intended for
/// commands that produce a SMALL amount of output (well under the ~64 KiB pipe
/// buffer). All current callers (pgrep/ps/kill/which/umount/sshfs) qualify; a
/// child that floods stdout could block on a full pipe and then be killed on
/// the deadline — acceptable for these helpers, not a general streaming runner.
pub fn run_cmd_bounded(program: &str, args: &[&str], timeout: Duration) -> Option<Output> {
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("run_cmd_bounded: spawn {program} failed: {e}");
            return None;
        }
    };

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Child reaped by try_wait; drain the (small) buffered output
                // WITHOUT waiting for EOF (a daemonized grandchild may still
                // hold the pipe open — read_to_end would hang forever).
                let stdout = child.stdout.take().map(|mut s| drain_nonblocking(&mut s)).unwrap_or_default();
                let stderr = child.stderr.take().map(|mut s| drain_nonblocking(&mut s)).unwrap_or_default();
                return Some(Output { status, stdout, stderr });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    warn!(
                        "run_cmd_bounded: {program} exceeded {}s — killing",
                        timeout.as_secs()
                    );
                    let _ = child.kill();
                    let _ = child.wait(); // reap so no zombie
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                warn!("run_cmd_bounded: try_wait {program} failed: {e}");
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Target soft limit for open file descriptors.
///
/// launchd starts user agents with a soft `RLIMIT_NOFILE` of only **256**,
/// which is far too low for a long-lived daemon that simultaneously holds an
/// IPC listener, up to `MAX_CONNS` accepted client connections, per-host SSH
/// ControlMaster sockets, pty master/slave fds during each login, retained
/// `ssh -L` / `ssh -N` tunnel child pipes, and the event channel. Under load
/// (or a transient fd spike during reconnect churn) 256 is exhausted, after
/// which **every** subsequent `spawn ssh` fails with "Too many open files" /
/// "dup of fd failed" — which then drives an endless restart + credential-read
/// storm (the failure observed in production).
///
/// We raise the SOFT limit toward this target, capped at the inherited HARD
/// limit (we never exceed it — that would need privileges) and at what the
/// kernel will actually grant (`setrlimit` is retried at smaller values).
const TARGET_NOFILE: u64 = 8192;

/// Candidate soft limits, tried high-to-low. macOS clamps `RLIMIT_NOFILE` to
/// `kern.maxfilesperproc`, so `setrlimit` to a large value can fail; we fall
/// back to progressively smaller targets so we always end up at the highest
/// value the kernel will grant.
const CANDIDATES: [u64; 4] = [TARGET_NOFILE, 4096, 2048, 1024];

/// Raise the process's soft `RLIMIT_NOFILE` toward [`TARGET_NOFILE`], capped at
/// the hard limit and at what the kernel grants. Returns `(old_soft, new_soft)`
/// on a successful raise, or `None` if the limit was already adequate or could
/// not be changed.
///
/// Best-effort and idempotent: any failure is logged and ignored (the daemon
/// still runs, just with the inherited limit). Call once at startup.
pub fn raise_fd_limit() -> Option<(u64, u64)> {
    // SAFETY: `get`/`setrlimit` are called with a valid resource id and a
    // properly-initialised `rlimit` struct; standard libc calls, no aliasing.
    unsafe {
        let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            warn!("getrlimit(RLIMIT_NOFILE) failed; keeping inherited fd limit");
            return None;
        }
        let old_soft = lim.rlim_cur as u64;
        let hard = lim.rlim_max as u64;
        let infinity = libc::RLIM_INFINITY as u64;

        // The most we may ask for: capped at the hard limit unless it's
        // unlimited.
        let ceiling = if hard == infinity { TARGET_NOFILE } else { hard };

        for &cand in CANDIDATES.iter() {
            let desired = cand.min(ceiling);
            if desired <= old_soft {
                continue; // already at least this high — nothing to gain
            }
            lim.rlim_cur = desired as libc::rlim_t;
            if libc::setrlimit(libc::RLIMIT_NOFILE, &lim) == 0 {
                info!("raised RLIMIT_NOFILE soft limit {old_soft} -> {desired} (hard {hard})");
                return Some((old_soft, desired));
            }
        }

        if old_soft < TARGET_NOFILE {
            warn!(
                "could not raise RLIMIT_NOFILE above soft={old_soft} (hard={hard}); \
                 keeping inherited limit"
            );
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current_soft() -> u64 {
        // SAFETY: getrlimit with a valid resource id and init'd struct.
        unsafe {
            let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim), 0);
            lim.rlim_cur as u64
        }
    }

    #[test]
    fn raise_fd_limit_never_lowers_and_reports_truthfully() {
        let before = current_soft();
        let result = raise_fd_limit();
        let after = current_soft();

        // The soft limit must never go DOWN as a result of calling this.
        assert!(
            after >= before,
            "fd limit must not decrease: before={before} after={after}"
        );

        match result {
            Some((old, new)) => {
                // A reported raise must be a genuine increase, and the live
                // limit must reflect at least the new value.
                assert_eq!(old, before, "reported old soft must match pre-call value");
                assert!(new > old, "reported raise must be a real increase");
                assert!(
                    after >= new,
                    "live soft limit {after} must be >= reported new {new}"
                );
            }
            None => {
                // No raise reported → limit unchanged.
                assert_eq!(after, before, "no-op call must leave the limit unchanged");
            }
        }
    }

    #[test]
    fn run_cmd_bounded_returns_output_for_fast_command() {
        let out = run_cmd_bounded("/bin/echo", &["hello"], Duration::from_secs(5))
            .expect("echo should complete well within timeout");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
    }

    #[test]
    fn run_cmd_bounded_kills_on_deadline() {
        let start = Instant::now();
        let out = run_cmd_bounded("/bin/sleep", &["10"], Duration::from_millis(300));
        let elapsed = start.elapsed();
        assert!(out.is_none(), "a 10s sleep must be killed by a 300ms deadline");
        assert!(
            elapsed < Duration::from_secs(3),
            "must return shortly after the deadline, not after the child's full runtime (elapsed {elapsed:?})"
        );
    }

    #[test]
    fn run_cmd_bounded_does_not_hang_when_grandchild_holds_pipe() {
        // Reproduce the sshfs/daemonize shape: the parent prints output then
        // exits, but a backgrounded grandchild INHERITS (and holds) the stdout
        // pipe. read_to_end would block until the grandchild dies (~5s); the
        // non-blocking drain must return the buffered output promptly.
        let start = Instant::now();
        let out = run_cmd_bounded(
            "/bin/sh",
            &["-c", "echo done; sleep 5 &"],
            Duration::from_secs(10),
        )
        .expect("parent exits immediately; must return Some");
        let elapsed = start.elapsed();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "done");
        assert!(
            elapsed < Duration::from_secs(2),
            "must not block on the grandchild holding the pipe (elapsed {elapsed:?})"
        );
    }

    #[test]
    fn run_cmd_bounded_none_for_missing_program() {
        let out = run_cmd_bounded("/nonexistent/program/zzz", &[], Duration::from_secs(1));
        assert!(out.is_none(), "a missing program must yield None, not panic");
    }

    #[test]
    fn raise_fd_limit_is_idempotent() {
        let _ = raise_fd_limit();
        let after_first = current_soft();
        // Second call should be a no-op (already adequate) and never lower it.
        let second = raise_fd_limit();
        let after_second = current_soft();
        assert!(after_second >= after_first);
        if after_first >= TARGET_NOFILE {
            assert!(
                second.is_none(),
                "once at/above target, a repeat call must report no raise"
            );
        }
    }
}
