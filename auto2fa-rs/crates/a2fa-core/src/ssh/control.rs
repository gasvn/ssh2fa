//! SSH ControlMaster socket path helpers and control-channel commands.
//!
//! Mirrors `get_ssh_control_path`, `update_symlink`, `cleanup_stale_connection`,
//! and the heartbeat `ssh -O check` / `ssh -O exit` calls in `backend.py`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use log::{info, warn};

/// Maximum time to wait for `ssh -O check` to respond.
///
/// A wedged control socket can hang the call indefinitely; we cap it at 5 s
/// and treat timeout as "master not alive".
const MASTER_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// ControlPath scheme
// ---------------------------------------------------------------------------

/// Return the **pool-member** ControlPath for a given host and pool index.
///
/// Mirrors the Python expression:
/// ```python
/// self.pool_control_paths = {
///     i: f"{self.target_control_path}-{i}" for i in range(POOL_SIZE)
/// }
/// ```
/// where `target_control_path` defaults to `~/.ssh/cm-auto2fa-<host>`.
///
/// The active symlink (`target_control_path`) is stored **without** the
/// `-<index>` suffix and is managed separately by `update_symlink`.
pub fn control_path(host: &str, index: usize) -> PathBuf {
    let home = dirs_home();
    home.join(".ssh").join(format!("cm-auto2fa-{host}-{index}"))
}

/// Return the **active symlink** path (no pool index suffix).
///
/// ssh clients use this path; `update_symlink` keeps it pointing at the
/// currently-active pool member.
pub fn active_symlink_path(host: &str) -> PathBuf {
    let home = dirs_home();
    home.join(".ssh").join(format!("cm-auto2fa-{host}"))
}

// ---------------------------------------------------------------------------
// Active-symlink management (mirrors `update_symlink` in backend.py)
// ---------------------------------------------------------------------------

/// Point the active symlink at the specified pool member socket, atomically.
///
/// Uses a temp-link + `rename` so callers never see a broken/absent symlink.
/// Returns `true` on success.
pub fn update_symlink(host: &str, index: usize) -> bool {
    let target = active_symlink_path(host);
    let source = control_path(host, index);
    let tmp_link = target.with_extension("tmp");

    // Ensure source path is absolute
    let abs_source = match source.canonicalize() {
        Ok(p) => p,
        // socket may not exist yet (master not started); use as-is
        Err(_) => source.clone(),
    };

    // Clean up stale tmp link
    let _ = std::fs::remove_file(&tmp_link);

    if let Err(e) = std::os::unix::fs::symlink(&abs_source, &tmp_link) {
        warn!("[{host}] Failed to create tmp symlink: {e}");
        return false;
    }
    if let Err(e) = std::fs::rename(&tmp_link, &target) {
        warn!("[{host}] Failed to atomically replace symlink: {e}");
        let _ = std::fs::remove_file(&tmp_link);
        return false;
    }
    info!("[{host}] Rotated symlink → pool {index}");
    true
}

/// Remove the active symlink and both pool-member socket files for `host`.
pub fn remove_symlink(host: &str) {
    let target = active_symlink_path(host);
    let _ = std::fs::remove_file(&target);
}

// ---------------------------------------------------------------------------
// Control-channel commands (ssh -O …)
// ---------------------------------------------------------------------------

/// Run `ssh -O check -o ControlPath=<path> <host>` and return `true` iff
/// exit code 0 (master is alive and responding).
///
/// This is the *local* check used by the heartbeat — it does NOT send a
/// network round-trip; it just asks the local ControlMaster process for
/// its status. Normally returns in milliseconds.
///
/// A [`MASTER_CHECK_TIMEOUT`] (5 s) is enforced: if the child has not exited
/// within that window (e.g. wedged control socket), the process is killed and
/// `false` is returned. This prevents the tick thread from hanging forever.
pub fn master_check(control_path: &Path, host: &str) -> bool {
    let mut child = match Command::new("ssh")
        .args([
            "-O",
            "check",
            "-o",
            &format!("ControlPath={}", control_path.display()),
            host,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("[{host}] ssh -O check spawn failed: {e}");
            return false;
        }
    };

    let deadline = Instant::now() + MASTER_CHECK_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    warn!("[{host}] ssh -O check timed out after {MASTER_CHECK_TIMEOUT:?} — killing");
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                warn!("[{host}] ssh -O check wait error: {e}");
                let _ = child.kill();
                return false;
            }
        }
    }
}

/// Send `ssh -O exit` to cleanly shut down the ControlMaster for a pool slot.
///
/// Failures are logged but not propagated — an exit may legitimately fail if
/// the master is already dead.
pub fn master_exit(control_path: &Path, host: &str) {
    let res = Command::new("ssh")
        .args([
            "-O",
            "exit",
            "-o",
            &format!("ControlPath={}", control_path.display()),
            host,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match res {
        Ok(s) if s.success() => info!("[{host}] ControlMaster exited cleanly"),
        Ok(s) => warn!("[{host}] ssh -O exit returned {s} (master may already be dead)"),
        Err(e) => warn!("[{host}] ssh -O exit failed: {e}"),
    }
}

/// Remove any stale socket file at `path`, optionally sending `ssh -O exit`
/// first (polite teardown). Mirrors `cleanup_stale_connection` in backend.py
/// minus the zombie-kill logic (that is handled at a higher layer).
pub fn cleanup_stale_socket(path: &Path, host: &str) {
    if path.exists() {
        // Polite exit (ignore errors — socket may be stale)
        let _ = Command::new("ssh")
            .args([
                "-o",
                &format!("ControlPath={}", path.display()),
                "-O",
                "exit",
                host,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        // Force-remove the socket file if still present
        if path.exists() {
            if let Err(e) = std::fs::remove_file(path) {
                warn!("[{host}] Could not remove stale socket {}: {e}", path.display());
            } else {
                info!("[{host}] Removed stale socket {}", path.display());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_path_is_stable_per_host_index() {
        let a = control_path("k6", 0);
        let b = control_path("k6", 0);
        assert_eq!(a, b);
        assert!(a.to_string_lossy().contains("k6"));
        assert_ne!(control_path("k6", 0), control_path("k6", 1));
    }

    #[test]
    fn control_path_contains_host_and_index() {
        let p = control_path("cannon", 1);
        let s = p.to_string_lossy();
        assert!(s.contains("cannon"), "expected host in path: {s}");
        assert!(s.contains("-1"), "expected index in path: {s}");
        assert!(s.contains("cm-auto2fa-"), "expected prefix in path: {s}");
    }

    #[test]
    fn active_symlink_has_no_index_suffix() {
        let sym = active_symlink_path("k6");
        let pool0 = control_path("k6", 0);
        // The symlink path must not end with "-0" or "-1"
        let sym_s = sym.to_string_lossy();
        assert!(!sym_s.ends_with("-0"), "symlink should have no index: {sym_s}");
        // The pool path must end with "-0"
        let p0 = pool0.to_string_lossy();
        assert!(p0.ends_with("-0"), "pool path should end with -0: {p0}");
    }

    #[test]
    fn control_path_different_hosts_differ() {
        assert_ne!(control_path("k6", 0), control_path("cannon", 0));
    }

    /// `master_check` with a bogus (non-existent) control socket must return
    /// `false` quickly — well within the 5 s timeout.
    #[test]
    fn master_check_bogus_path_returns_false_quickly() {
        use std::time::Instant;
        let bogus = std::path::Path::new("/tmp/bogus-auto2fa-test-socket-does-not-exist");
        let t0 = Instant::now();
        let result = master_check(bogus, "localhost");
        let elapsed = t0.elapsed();
        assert!(!result, "bogus path must return false");
        // ssh -O check fails immediately for a missing socket — must be << 5 s.
        assert!(
            elapsed < std::time::Duration::from_secs(4),
            "master_check took too long: {elapsed:?}"
        );
    }
}
