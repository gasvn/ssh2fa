import Foundation

/// Mirror of the daemon's tunnel snapshot.
struct Tunnel: Identifiable, Codable, Equatable, Hashable {
    let name: String
    let localPort: Int
    let remotePort: Int
    let jumpCandidates: [String]?
    let lastNode: String?
    let lastUser: String?
    let autoStart: Bool
    let postConnectCmd: String?
    let tags: [String]
    let activeJump: String?
    let status: String
    let lastMsg: String
    let lastAliveAt: Double

    var id: String { name }
    var url: String { "localhost:\(localPort)" }

    enum CodingKeys: String, CodingKey {
        case name, status, tags
        case localPort = "local_port"
        case remotePort = "remote_port"
        case jumpCandidates = "jump_candidates"
        case lastNode = "last_node"
        case lastUser = "last_user"
        case autoStart = "auto_start"
        case postConnectCmd = "post_connect_cmd"
        case activeJump = "active_jump"
        case lastMsg = "last_msg"
        case lastAliveAt = "last_alive_at"
    }

    // Defaults so older daemon snapshots still decode.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.name = try c.decode(String.self, forKey: .name)
        self.status = try c.decode(String.self, forKey: .status)
        self.localPort = try c.decode(Int.self, forKey: .localPort)
        self.remotePort = try c.decode(Int.self, forKey: .remotePort)
        self.jumpCandidates = try c.decodeIfPresent([String].self, forKey: .jumpCandidates)
        self.lastNode = try c.decodeIfPresent(String.self, forKey: .lastNode)
        self.lastUser = try c.decodeIfPresent(String.self, forKey: .lastUser)
        self.autoStart = try c.decode(Bool.self, forKey: .autoStart)
        self.postConnectCmd = try c.decodeIfPresent(String.self, forKey: .postConnectCmd)
        self.tags = (try? c.decode([String].self, forKey: .tags)) ?? []
        self.activeJump = try c.decodeIfPresent(String.self, forKey: .activeJump)
        self.lastMsg = try c.decode(String.self, forKey: .lastMsg)
        self.lastAliveAt = (try? c.decode(Double.self, forKey: .lastAliveAt)) ?? 0
    }

    /// Human-friendly "alive 2h", "last alive 5m ago", "never alive".
    /// Negative numbers handled defensively even though they shouldn't happen.
    func aliveSince(now: Date = Date()) -> String? {
        guard lastAliveAt > 0 else { return nil }
        let elapsed = now.timeIntervalSince1970 - lastAliveAt
        let abs = max(0, elapsed)
        let prefix = (displayState == .alive) ? "alive" : "last alive"
        let suffix = (displayState == .alive) ? "" : " ago"
        let unit: String
        if abs < 60 { unit = "\(Int(abs))s" }
        else if abs < 3600 { unit = "\(Int(abs/60))m" }
        else if abs < 86400 { unit = "\(Int(abs/3600))h" }
        else { unit = "\(Int(abs/86400))d" }
        return "\(prefix) \(unit)\(suffix)"
    }

    enum DisplayState: String {
        case alive, starting, stale, idle, portBusy, failed, unknown

        init(from raw: String) {
            switch raw {
            case "alive": self = .alive
            case "starting": self = .starting
            case "stale": self = .stale
            case "idle": self = .idle
            case "port_busy": self = .portBusy
            case "failed": self = .failed
            default: self = .unknown
            }
        }
    }

    var displayState: DisplayState { DisplayState(from: status) }
}
