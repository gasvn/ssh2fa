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
    }

    // Defaults so older daemon snapshots (pre-tags) still decode.
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
