import Foundation

/// Mirror of the daemon's host snapshot. Named `SSHHost` to avoid colliding
/// with `Foundation.Host` (NSHost).
struct SSHHost: Identifiable, Codable, Equatable, Hashable {
    let host: String
    let status: String
    let active: Bool
    let isMasterReady: Bool
    let poolIndex: Int
    let poolAlive: Int
    let isMounted: Bool
    let lastMsg: String

    var id: String { host }

    enum CodingKeys: String, CodingKey {
        case host, status, active
        case isMasterReady = "is_master_ready"
        case poolIndex = "pool_index"
        case poolAlive = "pool_alive"
        case isMounted = "is_mounted"
        case lastMsg = "last_msg"
    }

    enum DisplayState {
        case connected, connecting, failed, stopped, unknown
    }

    var displayState: DisplayState {
        let lc = status.lowercased()
        // ORDER MATTERS — check negative words before the substrings they
        // contain ("inactive" contains "active", "reconnect failed" contains
        // "connect"): stopped/failed first, then connected.
        if lc.contains("stop") || lc.contains("inactive") || lc == "idle" {
            // The daemon's stopped status is the literal "Idle" — it used to
            // fall through every branch and render as "?" in the menu bar.
            return .stopped
        }
        if lc.contains("fail") || lc.contains("error") || lc.contains("crash") {
            return .failed
        }
        if lc.contains("init") || lc.contains("connecting") || lc.contains("spawn")
            || lc.contains("starting") || lc.contains("cooldown") || lc.contains("reconnect") {
            // "Cooldown"/"Cooldown (Ns)" = rate-limit sit-out before the next
            // attempt → show as in-progress, not "?".
            return .connecting
        }
        if lc.contains("connected") || (lc.contains("active") && !lc.contains("fail")) {
            return .connected
        }
        return .unknown
    }
}
