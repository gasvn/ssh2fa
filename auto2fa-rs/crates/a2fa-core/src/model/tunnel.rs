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

    /// Last compute node this tunnel targeted (e.g. "gpunode01").
    pub last_node: Option<String>,

    /// Last UNIX user used for the `user@node` part of the ssh command.
    pub last_user: Option<String>,

    /// When `Some(host)`, this tunnel forwards local_port → localhost:remote_port
    /// directly ON that registered host (`ssh -N -L … <host>`) — NO jump host and
    /// NO SLURM compute node. `None` = the default SLURM compute-node forward.
    ///
    /// `#[serde(default)]`: old tunnels.json and old IPC snapshots omit this field
    /// and must still decode (→ None = unchanged compute behavior).
    #[serde(default)]
    pub direct_host: Option<String>,

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
    ///
    /// `#[serde(default)]`: the IPC `tunnel_snapshot` deliberately OMITS this
    /// field (clients shouldn't depend on it), but the TUI deserializes
    /// snapshots into this very struct — without the default, every
    /// `list_tunnels` decode failed on "missing field wants_alive" and the
    /// TUI's tunnel pane stayed permanently empty.
    #[serde(default)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A tunnel JSON WITHOUT direct_host must decode (→ None) so old
    /// tunnels.json / old snapshots keep working.
    #[test]
    fn direct_host_defaults_to_none_when_absent() {
        let json = r#"{
            "name": "nb", "local_port": 8888, "remote_port": 8888,
            "jump_candidates": null, "last_node": null, "last_user": null,
            "auto_start": false, "post_connect_cmd": null, "tags": [],
            "url_path": null, "status": "idle", "active_jump": null,
            "last_msg": "", "last_alive_at": 0.0, "total_uptime_sec": 0.0,
            "connect_count": 0, "fail_count": 0
        }"#;
        let t: Tunnel = serde_json::from_str(json).expect("decode without direct_host");
        assert_eq!(t.direct_host, None);
    }

    /// A direct tunnel round-trips its host.
    #[test]
    fn direct_host_round_trips() {
        let json = r#"{
            "name": "web", "local_port": 9000, "remote_port": 9000,
            "jump_candidates": null, "last_node": null, "last_user": null,
            "auto_start": false, "post_connect_cmd": null, "tags": [],
            "url_path": null, "direct_host": "loginhost", "status": "idle",
            "active_jump": null, "last_msg": "", "last_alive_at": 0.0,
            "total_uptime_sec": 0.0, "connect_count": 0, "fail_count": 0
        }"#;
        let t: Tunnel = serde_json::from_str(json).expect("decode with direct_host");
        assert_eq!(t.direct_host.as_deref(), Some("loginhost"));
        let back = serde_json::to_value(&t).unwrap();
        assert_eq!(back["direct_host"], "loginhost");
    }
}
