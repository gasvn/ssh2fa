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
            if t.isDirect {
                // Direct tunnels have no compute node — surface the daemon's own
                // message (e.g. "waiting for host …") or a plain Idle, never the
                // "pick a node" instruction (there's no node control for them).
                return t.lastMsg.isEmpty ? "Idle" : t.lastMsg
            }
            if t.lastNode == nil { return "Pick a compute node to start" }
            return "Idle"
        case .portBusy:
            return "Port \(t.localPort) already in use"
        case .failed:
            return t.lastMsg.isEmpty
                ? "Failed — see activity log"
                : friendlyError(t.lastMsg)
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

    /// Translate common ssh / network errors into actionable plain English.
    /// Used in the connection-error banner + per-tunnel "failed" subtext —
    /// users shouldn't have to grok cryptic ssh stderr to know what to do
    /// next.
    static func friendlyError(_ raw: String) -> String {
        let lc = raw.lowercased()
        if lc.contains("connection refused") {
            return "Server not accepting connections — sshd is down or wrong port."
        }
        if lc.contains("no route to host") {
            return "Can't reach the server — check Wi-Fi or VPN."
        }
        if lc.contains("connection reset") || lc.contains("broken pipe") {
            return "Connection dropped — server restarted or network changed."
        }
        if lc.contains("operation timed out") || lc.contains("connect timed out") {
            return "Server didn't respond — network is slow, or server is unreachable."
        }
        if lc.contains("permission denied") {
            return "Login rejected — password or OTP is wrong. Re-add the host to fix."
        }
        if lc.contains("host key verification failed") {
            return "Server identity changed — see daemon log; may be a MITM or a server rebuild."
        }
        if lc.contains("rate-limit") || lc.contains("rate limit") || lc.contains("cool-down") {
            return "Server is rate-limiting too many failed logins — sitting out for a few minutes."
        }
        if lc.contains("daemon unreachable") || lc.contains("not connected") {
            return "Background helper isn't running — restart SSH2FA to fix."
        }
        // Pass-through: caller's message was already user-friendly enough,
        // or we didn't have a translation. Avoid lying about what happened.
        return raw
    }
}
