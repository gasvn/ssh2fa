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
    let activeJump: String?
    let status: String
    let lastMsg: String

    var id: String { name }
    var url: String { "localhost:\(localPort)" }

    enum CodingKeys: String, CodingKey {
        case name, status
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
