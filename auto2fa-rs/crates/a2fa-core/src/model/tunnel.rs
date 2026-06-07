use serde::{Deserialize, Serialize};

use crate::model::status::TunnelStatus;

/// A snapshot of a single tunnel as emitted by `_tunnel_snapshot` in daemon.py,
/// augmented with the persisted fields from `TunnelState` in tunnels.py.
///
/// Ports are `u16` for direct serde compatibility with the JSON wire format.
/// Validation (1024..=65535) is enforced at creation boundaries (e.g. tunnel add),
/// not in this struct — keeping deserialization infallible for IPC payloads.
///
/// Timestamps (`last_alive_at`, `total_uptime_sec`) are `f64` (Unix epoch seconds)
/// matching Python's `time.time()` and `float` accumulator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tunnel {
    // ---- Persisted fields (tunnels.json) ------------------------------------

    /// Unique tunnel name / identifier.
    pub name: String,

    /// Local port the SSH -L forward binds on 127.0.0.1.
    pub local_port: u16,

    /// Remote port on the compute node that local_port forwards to.
    pub remote_port: u16,

    /// Explicit set of jump-host names to try, in priority order.
    /// `None` means "use any ready host in passwords.json".
    pub jump_candidates: Option<Vec<String>>,

    /// Last compute node this tunnel targeted (e.g. "holygpu01").
    pub last_node: Option<String>,

    /// Last UNIX user used for the `user@node` part of the ssh command.
    pub last_user: Option<String>,

    /// If true, the tunnel is automatically started at daemon boot.
    pub auto_start: bool,

    /// Optional `/bin/sh -c` command run when the tunnel first reaches "alive".
    pub post_connect_cmd: Option<String>,

    /// User-defined grouping tags (stored verbatim).
    pub tags: Vec<String>,

    /// Optional URL path/query suffix for "Open in browser" (e.g. `"?token=abc"`).
    pub url_path: Option<String>,

    /// Whether the user wants this tunnel alive right now.
    /// Persisted so auto-recovery survives daemon restarts.
    pub wants_alive: bool,

    // ---- Runtime / snapshot fields ------------------------------------------

    /// Current lifecycle status of the tunnel.
    pub status: TunnelStatus,

    /// Name of the jump host currently forwarding this tunnel, if alive.
    pub active_jump: Option<String>,

    /// Human-readable last status message.
    pub last_msg: String,

    /// Unix timestamp (seconds) of the last time this tunnel reached "alive".
    /// `0.0` means it has never been alive.
    pub last_alive_at: f64,

    /// Cumulative seconds spent in "alive" state (live-computed in daemon.py
    /// via `_live_uptime` to include the current run).
    pub total_uptime_sec: f64,

    /// Number of successful "alive" transitions since daemon start.
    pub connect_count: u32,

    /// Number of failed/stale transitions since daemon start.
    pub fail_count: u32,
}
