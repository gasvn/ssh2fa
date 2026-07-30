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
        if lc.isEmpty { return String(localized: "Idle") }
        if lc.contains("pool active") { return String(localized: "Connected") }
        if lc.contains("failover") { return String(localized: "Switching") }
        if lc.contains("rotat") { return String(localized: "Switching") }
        if lc.contains("initializing") || lc.contains("init pool") { return String(localized: "Initializing") }
        if lc.contains("master 0 failed") { return String(localized: "Login failed") }
        if lc.contains("pool crashed") { return String(localized: "Crashed") }
        if lc.contains("stopped") { return String(localized: "Stopped") }
        return stripped
    }

    /// Translate raw last_msg (mgr.last_msg) — usually verbose internal
    /// progress strings. Keep the original if no rule matches so power
    /// users still get the technical message.
    static func hostLastMsg(_ raw: String) -> String {
        let s = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if s.isEmpty { return String(localized: "") }
        if s == "Inactive" { return String(localized: "Disabled") }
        if s == "Ready" { return String(localized: "Standing by") }
        if s.hasPrefix("Init Spawn #") { return String(localized: "Connecting (preparing)…") }
        if s.hasPrefix("Spawning #") { return String(localized: "Authenticating…") }
        if s.hasPrefix("Spawned #") { return String(localized: "Connected, finishing setup…") }
        if s.hasPrefix("Log Open #") { return String(localized: "Connected, finishing setup…") }
        if s.hasPrefix("Rotated ") || s.hasPrefix("Failover ") { return String(localized: "Switched to backup link") }
        if s.lowercased().contains("busy") { return String(localized: "Server busy — using backup") }
        return s
    }

    /// "alive 2h", "stale 5m", "idle", "connecting…", "failed — see log"
    static func tunnelStatusBlurb(_ t: Tunnel) -> String {
        switch t.displayState {
        case .alive:
            return t.aliveSince() ?? String(localized: "Connected")
        case .starting:
            return String(localized: "Connecting…")
        case .stale:
            // last_msg often "node gpunode8a15301 ended" — keep it.
            let m = t.lastMsg
            return m.isEmpty ? String(localized: "Compute node ended — pick a new one") : m
        case .idle:
            if t.isDirect {
                // Direct tunnels have no compute node — surface the daemon's own
                // message (e.g. "waiting for host …") or a plain Idle, never the
                // "pick a node" instruction (there's no node control for them).
                return t.lastMsg.isEmpty ? String(localized: "Idle") : t.lastMsg
            }
            if t.lastNode == nil { return String(localized: "Pick a compute node to start") }
            return String(localized: "Idle")
        case .portBusy:
            return String(localized: "Port \(t.localPort) already in use")
        case .failed:
            return t.lastMsg.isEmpty
                ? String(localized: "Failed — see activity log")
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
            return String(localized: "Server not accepting connections — sshd is down or wrong port.")
        }
        if lc.contains("no route to host") {
            return String(localized: "Can't reach the server — check Wi-Fi or VPN.")
        }
        if lc.contains("connection reset") || lc.contains("broken pipe") {
            return String(localized: "Connection dropped — server restarted or network changed.")
        }
        if lc.contains("operation timed out") || lc.contains("connect timed out") {
            return String(localized: "Server didn't respond — network is slow, or server is unreachable.")
        }
        if lc.contains("permission denied") {
            return String(localized: "Login rejected — password or OTP is wrong. Re-add the host to fix.")
        }
        if lc.contains("host key verification failed") {
            return String(localized: "Server identity changed — may be a server rebuild, or (rarely) a MITM.")
        }
        if lc.contains("rate-limit") || lc.contains("rate limit") || lc.contains("cool-down") {
            return String(localized: "Server is rate-limiting too many failed logins — sitting out for a few minutes.")
        }
        if lc.contains("daemon unreachable") || lc.contains("not connected") {
            return String(localized: "Background helper isn't running — restart SSH2FA to fix.")
        }
        // Pass-through: caller's message was already user-friendly enough,
        // or we didn't have a translation. Avoid lying about what happened.
        return raw
    }

    /// Translate a failure from a stored-credential read/write (the per-host
    /// "Password & setup" view) into something the user can act on.
    ///
    /// The important case is macOS Keychain authorization. After the app updates,
    /// the rebuilt background helper is a new binary to macOS, so the login
    /// Keychain asks permission once per saved item. The raw errors that surfaces
    /// — `keyring get(k8.password): Platform secure storage failure: User
    /// canceled the operation` — tell the user nothing about the one thing that
    /// fixes it: choosing **Always Allow** on that prompt.
    static func credentialError(_ raw: String) -> String {
        let lc = raw.lowercased()
        if lc.contains("user canceled") || lc.contains("user cancelled")
            || lc.contains("secure storage failure") {
            return String(localized: "macOS didn't allow access to the saved credential. Try again and choose “Always Allow” on the Keychain prompt — then it won't ask again.")
        }
        if lc.contains("timed out") {
            return String(localized: "macOS hasn't allowed access yet — a Keychain prompt may be waiting for you. Choose “Always Allow” there, then try again.")
        }
        if lc.contains("already in flight") {
            return String(localized: "Still finishing the previous attempt — try again in a moment.")
        }
        if lc.contains("keychain is locked") || lc.contains("keychain locked") {
            return String(localized: "Your login Keychain is locked — unlock it (log out and back in, or open Keychain Access) and try again.")
        }
        if lc.contains("unknown method") {
            return String(localized: "The background helper is an older version than the app. Quit and reopen SSH2FA so it restarts.")
        }
        return friendlyError(raw)
    }
}
