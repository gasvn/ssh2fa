use serde::{Deserialize, Serialize};

/// The lifecycle state of a tunnel.
///
/// Matches the status strings used by `TunnelState` in tunnels.py (line 95):
/// `"idle" | "starting" | "alive" | "stale" | "port_busy" | "failed"`
///
/// `serde(rename_all = "snake_case")` maps every variant correctly:
///   Idle → "idle", Starting → "starting", Alive → "alive",
///   Failed → "failed", PortBusy → "port_busy", Stale → "stale"
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TunnelStatus {
    #[default]
    Idle,
    Starting,
    Alive,
    Failed,
    PortBusy,
    Stale,
}


impl std::fmt::Display for TunnelStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TunnelStatus::Idle => "idle",
            TunnelStatus::Starting => "starting",
            TunnelStatus::Alive => "alive",
            TunnelStatus::Failed => "failed",
            TunnelStatus::PortBusy => "port_busy",
            TunnelStatus::Stale => "stale",
        };
        f.write_str(s)
    }
}

/// The host status string as emitted by `_host_snapshot` in daemon.py.
///
/// This is a Rich-markup display string, e.g. `"[green]Pool Active (0)[/green]"`,
/// `"[dim]Stopped[/dim]"`, `"[yellow]Cool-down 5s[/yellow]"`.
/// It is free-form and not suitable for exhaustive enum matching; model as `String`.
pub type HostStatus = String;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_status_serde_roundtrip() {
        let cases = [
            (TunnelStatus::Idle, "\"idle\""),
            (TunnelStatus::Starting, "\"starting\""),
            (TunnelStatus::Alive, "\"alive\""),
            (TunnelStatus::Failed, "\"failed\""),
            (TunnelStatus::PortBusy, "\"port_busy\""),
            (TunnelStatus::Stale, "\"stale\""),
        ];
        for (variant, expected_json) in cases {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert_eq!(serialized, expected_json, "serialize {:?}", variant);
            let deserialized: TunnelStatus = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, variant, "roundtrip {:?}", variant);
        }
    }
}
