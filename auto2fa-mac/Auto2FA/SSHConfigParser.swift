import Foundation

/// One concrete `Host` entry parsed from ~/.ssh/config.
struct ConfigHost: Equatable, Hashable {
    let alias: String
    let hostName: String?
    let user: String?
}

/// Pure parser for ~/.ssh/config. v1: top-level concrete `Host` blocks only —
/// wildcard/glob/negated patterns (`Host *`, `Host *.edu`, `Host !x`) are
/// skipped (we never multiplex or import a pattern). Tolerant of comments +
/// indentation + key case. Does NOT follow Include/Match. Foundation-only →
/// unit-tested headlessly.
enum SSHConfigParser {
    static func parse(_ text: String) -> [ConfigHost] {
        var out: [ConfigHost] = []
        var current: [String] = []     // aliases on the open Host line
        var hostName: String?
        var user: String?

        func flush() {
            for a in current {
                out.append(ConfigHost(alias: a, hostName: hostName, user: user))
            }
            current = []; hostName = nil; user = nil
        }

        for rawLine in text.split(separator: "\n", omittingEmptySubsequences: false) {
            var line = String(rawLine)
            if let hash = line.firstIndex(of: "#") { line = String(line[..<hash]) }
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.isEmpty { continue }
            let parts = trimmed.split(whereSeparator: { $0 == " " || $0 == "\t" })
            guard let keyword = parts.first else { continue }
            let values = parts.dropFirst().map(String.init)
            switch keyword.lowercased() {
            case "host":
                flush()
                current = values.filter {
                    !$0.contains("*") && !$0.contains("?") && !$0.hasPrefix("!")
                }
            case "hostname":
                if hostName == nil { hostName = values.first }
            case "user":
                if user == nil { user = values.first }
            default:
                break
            }
        }
        flush()
        return out
    }
}
