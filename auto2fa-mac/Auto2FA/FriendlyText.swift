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
        // Login/session diagnoses are produced by the daemon so CLI clients
        // also receive the exact cause. Run them through the same UI mapper as
        // tunnel/test-login failures; unknown messages still pass through.
        return friendlyError(s)
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
        if lc.contains("password rejected") || lc.contains("rejected the password") {
            return String(localized: "The server rejected the password. Open Password & setup, replace it, then run Test login.")
        }
        if lc.contains("verification code rejected") || lc.contains("otp rejected")
            || lc.contains("rejected the 2fa code") {
            return String(localized: "The server rejected the 2FA code. Check the saved secret and the Mac’s date/time, then update it in Password & setup.")
        }
        if lc.contains("too many authentication failures") || lc.contains("too many ssh keys") {
            return String(localized: "Too many SSH keys were tried first. Enable IdentitiesOnly for this host or remove unused keys from ssh-agent, then retry.")
        }
        if lc.contains("could not resolve hostname")
            || lc.contains("host name could not be resolved") {
            return String(localized: "The host name could not be resolved. Check HostName and connect the required VPN/DNS, then retry.")
        }
        if lc.contains("account locked") {
            return String(localized: "The SSH account is locked. Wait for the lockout period or ask the server administrator to unlock it before retrying.")
        }
        if lc.contains("account expired") {
            return String(localized: "The SSH account has expired. Ask the server administrator to renew it before retrying.")
        }
        if lc.contains("maxsessions") || lc.contains("session limit")
            || lc.contains("refused a new ssh session")
            || lc.contains("refused a new session")
            || lc.contains("mux_client_request_session") {
            return String(localized: "The server is connected but refused a new SSH session. Close old SSH sessions; if it persists, ask the server administrator to check MaxSessions/PAM limits.")
        }
        if lc.contains("connection refused") {
            return String(localized: "Server not accepting connections — sshd is down or wrong port.")
        }
        if lc.contains("no route to host") {
            return String(localized: "Can't reach the server — check Wi-Fi or VPN.")
        }
        if lc.contains("connection reset") || lc.contains("broken pipe") {
            return String(localized: "Connection dropped — server restarted or network changed.")
        }
        if lc.contains("daemon timed out") {
            return String(localized: "SSH2FA is still starting or recovering. Wait a moment and try again.")
        }
        if lc.contains("operation timed out") || lc.contains("connect timed out")
            || lc.contains("connection timed out") {
            return String(localized: "Server didn't respond — network is slow, or server is unreachable.")
        }
        if lc.contains("permission denied") {
            return String(localized: "Login rejected — update the password or 2FA secret in Password & setup, then run Test login.")
        }
        if lc.contains("host key verification failed") {
            return String(localized: "The server identity key changed. Verify its fingerprint with the server administrator before updating known_hosts.")
        }
        if lc.contains("rate-limit") || lc.contains("rate limit") || lc.contains("cool-down") {
            return String(localized: "Server is rate-limiting too many failed logins — sitting out for a few minutes.")
        }
        if lc.contains("daemon unreachable") || lc.contains("not connected") {
            return String(localized: "SSH2FA isn't ready — quit and reopen the app. Use Troubleshoot if the problem continues.")
        }
        // Pass-through: caller's message was already user-friendly enough,
        // or we didn't have a translation. Avoid lying about what happened.
        return raw
    }

    /// True iff a daemon error means "this host has no 2FA secret" — as opposed
    /// to a read that failed (locked Keychain, pending prompt, busy worker).
    ///
    /// The distinction decides whether a UI element should disappear or stay and
    /// offer a retry: a password-only host has nothing to show and never will,
    /// while a failed read is worth trying again. Matching is deliberately
    /// narrow — the daemon's own phrasing is `no 2FA secret for <host>` — so a
    /// transient failure is never mistaken for a permanent absence.
    static func indicatesNoOTPSecret(_ raw: String) -> Bool {
        let lc = raw.lowercased()
        return lc.contains("no 2fa secret for") || lc.contains("no otp secret for")
    }

    /// Translate a failure from a stored-credential read/write (the per-host
    /// "Password & setup" view) into something the user can act on.
    ///
    /// The important case is the one-time migration from a legacy Keychain item
    /// into the stable daemon-owned vault. The raw framework error does not tell
    /// the user that allowing the old read once is enough to finish the move.
    static func credentialError(_ raw: String) -> String {
        let lc = raw.lowercased()
        if lc.contains("user canceled") || lc.contains("user cancelled")
            || lc.contains("secure storage failure") {
            return String(localized: "macOS didn't allow access to the old saved credential. Try again and choose “Allow” on the Keychain prompt so SSH2FA can finish the one-time migration.")
        }
        if lc.contains("timed out") {
            return String(localized: "macOS hasn't allowed access yet — a Keychain prompt may be waiting for you. Choose “Allow” there, then try again.")
        }
        if lc.contains("already in flight") {
            return String(localized: "Still finishing the previous attempt — try again in a moment.")
        }
        if lc.contains("keychain is locked") || lc.contains("keychain locked") {
            return String(localized: "Your login Keychain is locked — unlock it (log out and back in, or open Keychain Access) and try again.")
        }
        if lc.contains("unknown method") {
            return String(localized: "Part of SSH2FA is still on an older version. Quit and reopen the app to finish the update.")
        }
        return friendlyError(raw)
    }
}
