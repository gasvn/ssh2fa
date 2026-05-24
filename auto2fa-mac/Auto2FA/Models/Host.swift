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
        if lc.contains("connected") || (lc.contains("active") && !lc.contains("fail")) {
            return .connected
        }
        if lc.contains("init") || lc.contains("connecting") || lc.contains("spawn") || lc.contains("starting") {
            return .connecting
        }
        if lc.contains("fail") || lc.contains("error") || lc.contains("crash") {
            return .failed
        }
        if lc.contains("stop") || lc.contains("inactive") {
            return .stopped
        }
        return .unknown
    }
}
