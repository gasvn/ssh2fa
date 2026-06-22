use serde::{Deserialize, Serialize};

/// A snapshot of a single SSH jump-host as emitted by `_host_snapshot` in daemon.py.
///
/// Field names match the JSON keys exactly:
/// `host`, `status`, `active`, `is_master_ready`, `pool_index`,
/// `pool_alive`, `is_mounted`, `last_msg`.
///
/// `status` is a Rich-markup display string (free-form); see `HostStatus`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    /// The host alias / name (e.g. "k6", "gpunode02").
    pub host: String,

    /// Rich-markup display string, e.g. `"[green]Pool Active (0)[/green]"`.
    /// Free-form; do not match exhaustively.
    pub status: String,

    /// Whether the host is enabled by the user (autoConnect / active toggle).
    pub active: bool,

    /// True when the SSH ControlMaster socket is live and accepting connections.
    pub is_master_ready: bool,

    /// Index of the currently active connection-pool slot (0 or 1).
    pub pool_index: u8,

    /// Number of alive pexpect children in the pool (0, 1, or 2).
    pub pool_alive: u8,

    /// Whether the remote filesystem is currently SSHFS-mounted.
    pub is_mounted: bool,

    /// Human-readable last status message from the host manager.
    pub last_msg: String,
}

/// Canonical host-name safety check (mirrors `_valid_host_name` in daemon.py).
///
/// A host name flows UNQUOTED into ssh argv (as the final `<host>` argument)
/// and into filesystem paths (`/tmp/ssh2fa_ssh_master_<host>_N.log`,
/// `~/Mounts/<host>`). So it must NOT:
/// - start with `-` (ssh would parse it as an option — argument injection),
/// - start with `.` (hidden / `..` traversal),
/// - contain `..` (path traversal),
/// - contain anything outside `[A-Za-z0-9._-]` (path separators, shell
///   metacharacters, whitespace, NUL).
///
/// This is the SINGLE definition; both `host_add` validation and the
/// State-load filter use it, so a host name can never reach an argv/path sink
/// without having passed here.
pub fn is_safe_host_name(host: &str) -> bool {
    if host.is_empty() || host.contains("..") {
        return false;
    }
    let mut chars = host.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if !first.is_ascii_alphanumeric() && first != '_' {
        return false;
    }
    host.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::is_safe_host_name;

    #[test]
    fn safe_host_names() {
        assert!(is_safe_host_name("k6"));
        assert!(is_safe_host_name("gpunode08.hpc.example.edu"));
        assert!(is_safe_host_name("node-1.cluster"));
        assert!(is_safe_host_name("_x"));
    }

    #[test]
    fn unsafe_host_names_rejected() {
        assert!(!is_safe_host_name(""));
        assert!(!is_safe_host_name("-oProxyCommand=evil")); // ssh option injection
        assert!(!is_safe_host_name("a/b")); // path separator
        assert!(!is_safe_host_name("../etc")); // traversal
        assert!(!is_safe_host_name(".hidden"));
        assert!(!is_safe_host_name("a b")); // whitespace
        assert!(!is_safe_host_name("a;b")); // shell metachar
        assert!(!is_safe_host_name("a\nb")); // newline
    }
}
