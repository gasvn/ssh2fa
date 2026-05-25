import Foundation
import AppKit

/// Maps daemon-internal strings (which leak ControlMaster pool jargon
/// like "Pool Active (0)" / "Rotated 0->1" / "Init Spawn #0...") to
/// user-friendly English. Used by HostsView's status badge and the
/// "last message" column so non-engineers can read it.
enum FriendlyText {
    /// Translate raw host status (mgr.status). Strips rich-markup brackets
    /// the daemon still uses (`[green]Pool Active (0)[/green]`).
    static func hostStatus(_ raw: String) -> String {
        let stripped = raw.replacingOccurrences(of: "\\[/?[^\\]]+\\]",
                                                with: "",
                                                options: .regularExpression)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let lc = stripped.lowercased()
        if lc.isEmpty { return "Idle" }
        if lc.contains("pool active") { return "Connected" }
        if lc.contains("failover") { return "Switching" }
        if lc.contains("rotat") { return "Switching" }
        if lc.contains("initializing") || lc.contains("init pool") { return "Initializing" }
        if lc.contains("master 0 failed") { return "Login failed" }
        if lc.contains("pool crashed") { return "Crashed" }
        if lc.contains("stopped") { return "Stopped" }
        return stripped
    }

    /// Translate raw last_msg (mgr.last_msg) — usually verbose internal
    /// progress strings. Keep the original if no rule matches so power
    /// users still get the technical message.
    static func hostLastMsg(_ raw: String) -> String {
        let s = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if s.isEmpty { return "" }
        if s == "Inactive" { return "Disabled" }
        if s == "Ready" { return "Standing by" }
        if s.hasPrefix("Init Spawn #") { return "Connecting (preparing)…" }
        if s.hasPrefix("Spawning #") { return "Authenticating…" }
        if s.hasPrefix("Spawned #") { return "Connected, finishing setup…" }
        if s.hasPrefix("Log Open #") { return "Connected, finishing setup…" }
        if s.hasPrefix("Rotated ") || s.hasPrefix("Failover ") { return "Switched to backup link" }
        if s.lowercased().contains("busy") { return "Server busy — using backup" }
        return s
    }

    /// "alive 2h", "stale 5m", "idle", "connecting…", "failed — see log"
    static func tunnelStatusBlurb(_ t: Tunnel) -> String {
        switch t.displayState {
        case .alive:
            return t.aliveSince() ?? "Connected"
        case .starting:
            return "Connecting…"
        case .stale:
            // last_msg often "node holygpu8a15301 ended" — keep it.
            let m = t.lastMsg
            return m.isEmpty ? "Compute node ended — pick a new one" : m
        case .idle:
            if t.lastNode == nil { return "Pick a compute node to start" }
            return "Idle"
        case .portBusy:
            return "Port \(t.localPort) already in use"
        case .failed:
            return t.lastMsg.isEmpty ? "Failed — see activity log" : t.lastMsg
        case .unknown:
            return t.lastMsg
        }
    }

    /// Briefly play the macOS "alignment" haptic feedback on the trackpad
    /// (only fires on built-in trackpads with Force Touch). No-op elsewhere.
    static func haptic() {
        NSHapticFeedbackManager.defaultPerformer.perform(.alignment,
                                                          performanceTime: .now)
    }
}
