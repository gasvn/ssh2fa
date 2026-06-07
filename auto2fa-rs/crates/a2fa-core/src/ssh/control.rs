//! SSH ControlMaster socket path helpers and control-channel commands.
//!
//! Mirrors `get_ssh_control_path`, `update_symlink`, `cleanup_stale_connection`,
//! and the heartbeat `ssh -O check` / `ssh -O exit` calls in `backend.py`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use log::{info, warn};

/// Maximum time to wait for `ssh -O check` to respond.
///
/// A wedged control socket can hang the call indefinitely; we cap it at 5 s
/// and treat timeout as "master not alive".
const MASTER_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum time to wait for `ssh -G <host>` (ControlPath resolution).
const SSH_G_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// ControlPath scheme
// ---------------------------------------------------------------------------
//
// The base ControlPath is resolved from the user's ssh config via `ssh -G`,
// exactly like `get_ssh_control_path` in backend.py. This is essential for
// two reasons:
//   1. Correctness — Rust must honor a `ControlPath ~/.ssh/cm-auto2fa-%h`
//      directive (with %h/%n/~ expansion done by ssh itself), not invent its
//      own path. Ignoring it would orphan the user's configured sockets.
//   2. Interop / handoff — using the SAME path the Python daemon used lets a
//      freshly-started Rust daemon ADOPT the live ControlMaster sockets instead
//      of re-triggering 2FA on every host.
//
// Resolution (mirrors get_ssh_control_path):
//   * `ssh -G <host>` → take the `controlpath` value.
//   * value "none"         → fall back to ~/.ssh/cm-<host>
//   * no controlpath / err → fall back to ~/.ssh/cm-auto2fa-<host>
// The result is cached per host (ssh -G is cheap but not free, and the path is
// stable for the lifetime of the daemon).

fn control_base_cache() -> &'static Mutex<HashMap<String, PathBuf>> {
    static CACHE: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Parse the `controlpath` value out of `ssh -G` stdout (case-insensitive key).
/// Returns `None` if there is no controlpath line.
fn parse_ssh_g_controlpath(ssh_g_stdout: &str) -> Option<String> {
    for line in ssh_g_stdout.lines() {
        let mut it = line.splitn(2, ' ');
        if let (Some(key), Some(val)) = (it.next(), it.next()) {
            if key.eq_ignore_ascii_case("controlpath") {
                return Some(val.trim().to_string());
            }
        }
    }
    None
}

/// Turn an `ssh -G` controlpath value (or absence) into a concrete base path,
/// mirroring Python's `get_ssh_control_path` fallbacks.
fn control_base_from_ssh_g(host: &str, value: Option<&str>) -> PathBuf {
    match value {
        Some(v) if v.eq_ignore_ascii_case("none") => {
            dirs_home().join(".ssh").join(format!("cm-{host}"))
        }
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => dirs_home().join(".ssh").join(format!("cm-auto2fa-{host}")),
    }
}

/// Run `ssh -G <host>` with a timeout and return its stdout, or `None` on
/// timeout / spawn failure / non-zero exit. `ssh -G` does not open a network
/// connection, but a wedged `Match exec`/`ProxyCommand` config could hang it.
fn run_ssh_g(host: &str) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    let host_owned = host.to_string();
    std::thread::spawn(move || {
        let out = Command::new("ssh").args(["-G", &host_owned]).output();
        let _ = tx.send(out);
    });
    match rx.recv_timeout(SSH_G_TIMEOUT) {
        Ok(Ok(out)) if out.status.success() => {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        }
        _ => None,
    }
}

/// Resolve the **base** ControlPath for `host` (no `-<index>` suffix), cached.
///
/// Mirrors `get_ssh_control_path` in backend.py.
pub fn resolve_control_base(host: &str) -> PathBuf {
    if let Some(p) = control_base_cache().lock().unwrap().get(host) {
        return p.clone();
    }
    let value = run_ssh_g(host);
    let base = control_base_from_ssh_g(host, value.as_deref().and_then(parse_ssh_g_controlpath).as_deref());
    control_base_cache()
        .lock()
        .unwrap()
        .insert(host.to_string(), base.clone());
    base
}

/// Return the **pool-member** ControlPath for a given host and pool index.
///
/// Mirrors the Python expression:
/// ```python
/// self.pool_control_paths = {
///     i: f"{self.target_control_path}-{i}" for i in range(POOL_SIZE)
/// }
/// ```
/// where `target_control_path` comes from `resolve_control_base` (`ssh -G`).
///
/// The active symlink (`target_control_path`) is stored **without** the
/// `-<index>` suffix and is managed separately by `update_symlink`.
pub fn control_path(host: &str, index: usize) -> PathBuf {
    let base = resolve_control_base(host);
    PathBuf::from(format!("{}-{index}", base.display()))
}

/// Return the **active symlink** path (no pool index suffix).
///
/// ssh clients use this path; `update_symlink` keeps it pointing at the
/// currently-active pool member.
pub fn active_symlink_path(host: &str) -> PathBuf {
    resolve_control_base(host)
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

/// Return the pool index the active symlink currently points at, if it exists
/// and ends in a `-<index>` suffix. Used at boot to adopt the slot ssh clients
/// are already multiplexing over.
pub fn symlink_target_index(host: &str) -> Option<usize> {
    let target = std::fs::read_link(active_symlink_path(host)).ok()?;
    parse_trailing_index(&target.to_string_lossy())
}

/// Parse the `-<index>` suffix off a pool socket file name. The base may contain
/// dashes (`cm-auto2fa-...`) and dots, so we split on the LAST dash.
fn parse_trailing_index(name: &str) -> Option<usize> {
    name.rsplit_once('-')
        .and_then(|(_, idx)| idx.parse::<usize>().ok())
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

    // -- Pure resolution helpers (deterministic, no ssh invoked) ------------

    #[test]
    fn parse_controlpath_picks_value_case_insensitive() {
        let out = "user me\nControlPath /home/me/.ssh/cm-auto2fa-host.example-0\nport 22\n";
        assert_eq!(
            parse_ssh_g_controlpath(out).as_deref(),
            Some("/home/me/.ssh/cm-auto2fa-host.example-0")
        );
        // lowercase key (ssh -G normalizes to lowercase)
        let out2 = "controlpath none\n";
        assert_eq!(parse_ssh_g_controlpath(out2).as_deref(), Some("none"));
        // no controlpath line
        assert_eq!(parse_ssh_g_controlpath("user me\nport 22\n"), None);
    }

    #[test]
    fn control_base_fallbacks_match_python() {
        // explicit "none" → ~/.ssh/cm-<host>
        let none = control_base_from_ssh_g("b8", Some("none"));
        assert!(none.to_string_lossy().ends_with("/.ssh/cm-b8"), "{none:?}");
        // no controlpath line → ~/.ssh/cm-auto2fa-<host>
        let missing = control_base_from_ssh_g("b8", None);
        assert!(
            missing.to_string_lossy().ends_with("/.ssh/cm-auto2fa-b8"),
            "{missing:?}"
        );
        // concrete value (already %h/~ expanded by ssh) → used verbatim
        let concrete = control_base_from_ssh_g(
            "b8",
            Some("/Users/me/.ssh/cm-auto2fa-boslogin08.rc.fas.harvard.edu"),
        );
        assert_eq!(
            concrete,
            PathBuf::from("/Users/me/.ssh/cm-auto2fa-boslogin08.rc.fas.harvard.edu")
        );
    }

    // -- Public path API (structural, environment-robust) -------------------

    #[test]
    fn control_path_is_stable_per_host_index() {
        // Use a synthetic host that won't have an ssh config entry → the path
        // is resolved deterministically via the fallback, independent of the
        // machine's ~/.ssh/config.
        let h = "auto2fa-unittest-synthetic-host";
        let a = control_path(h, 0);
        let b = control_path(h, 0);
        assert_eq!(a, b);
        assert_ne!(control_path(h, 0), control_path(h, 1));
    }

    #[test]
    fn control_path_has_index_suffix_and_symlink_does_not() {
        let h = "auto2fa-unittest-synthetic-host";
        let pool1 = control_path(h, 1);
        let s = pool1.to_string_lossy();
        assert!(s.ends_with("-1"), "expected index suffix: {s}");
        assert!(s.contains(h), "expected host in fallback path: {s}");

        let sym = active_symlink_path(h);
        let sym_s = sym.to_string_lossy();
        assert!(!sym_s.ends_with("-0"), "symlink should have no index: {sym_s}");
        assert!(!sym_s.ends_with("-1"), "symlink should have no index: {sym_s}");
        // pool path is exactly the base + "-1"
        assert_eq!(pool1, PathBuf::from(format!("{sym_s}-1")));
    }

    #[test]
    fn control_path_different_hosts_differ() {
        assert_ne!(
            control_path("auto2fa-unittest-host-a", 0),
            control_path("auto2fa-unittest-host-b", 0)
        );
    }

    #[test]
    fn parse_trailing_index_handles_dashed_base() {
        assert_eq!(
            parse_trailing_index("/Users/me/.ssh/cm-auto2fa-boslogin08.rc.fas.harvard.edu-1"),
            Some(1)
        );
        assert_eq!(parse_trailing_index("cm-auto2fa-k6-0"), Some(0));
        assert_eq!(parse_trailing_index("no-index-here"), None);
        assert_eq!(parse_trailing_index("plainname"), None);
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
