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
    /// The host alias / name (e.g. "k6", "holygpu02").
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
