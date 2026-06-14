import Foundation

/// One concrete `Host` entry parsed from ~/.ssh/config.
struct ConfigHost: Equatable, Hashable {
    let alias: String
    let hostName: String?
    let user: String?
}

/// Result of parsing ~/.ssh/config: the concrete hosts, the wildcard `Host`
/// patterns we deliberately don't resolve (`Host gpu-*`), and whether the
/// config uses `Include`/`Match` (which means our top-level view is
/// incomplete). The latter two let the reconciliation warning stay quiet
/// instead of false-alarming on configs it can't fully see.
struct ParsedSSHConfig: Equatable {
    let hosts: [ConfigHost]
    let patterns: [String]
    let hasIncludeOrMatch: Bool

    static let empty = ParsedSSHConfig(hosts: [], patterns: [], hasIncludeOrMatch: false)
}

/// Pure parser for ~/.ssh/config. v1: top-level concrete `Host` blocks only —
/// wildcard/glob/negated patterns (`Host *`, `Host *.edu`, `Host !x`) are
/// recorded as `patterns` but never imported. Tolerant of comments +
/// indentation + key case + CRLF. Does NOT follow Include/Match (but flags
/// their presence). Foundation-only → unit-tested headlessly.
enum SSHConfigParser {
    /// Convenience: just the concrete hosts (back-compat for callers that only
    /// want the list).
    static func parse(_ text: String) -> [ConfigHost] { parseFull(text).hosts }

    static func parseFull(_ text: String) -> ParsedSSHConfig {
        var hosts: [ConfigHost] = []
        var patterns: [String] = []
        var hasIncludeOrMatch = false
        var current: [String] = []     // concrete aliases on the open Host line
        var hostName: String?
        var user: String?

        func flush() {
            for a in current {
                hosts.append(ConfigHost(alias: a, hostName: hostName, user: user))
            }
            current = []; hostName = nil; user = nil
        }

        // Normalize line endings first: Swift treats "\r\n" as ONE Character
        // grapheme, so split(separator: "\n") wouldn't break CRLF lines at all.
        let normalized = text.replacingOccurrences(of: "\r\n", with: "\n")
                             .replacingOccurrences(of: "\r", with: "\n")
        for rawLine in normalized.split(separator: "\n", omittingEmptySubsequences: false) {
            var line = String(rawLine)
            if let hash = line.firstIndex(of: "#") { line = String(line[..<hash]) }
            let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
            if trimmed.isEmpty { continue }
            let parts = trimmed.split(whereSeparator: { $0 == " " || $0 == "\t" })
            guard let keyword = parts.first else { continue }
            let values = parts.dropFirst().map(String.init)
            switch keyword.lowercased() {
            case "host":
                flush()
                for tok in values {
                    if tok.contains("*") || tok.contains("?") {
                        patterns.append(tok)        // positive glob → record for matching
                    } else if tok.hasPrefix("!") {
                        continue                    // negation → ignore (under-warn is safe)
                    } else {
                        current.append(tok)
                    }
                }
            case "hostname":
                if hostName == nil { hostName = values.first }
            case "user":
                if user == nil { user = values.first }
            case "include", "match":
                hasIncludeOrMatch = true
            default:
                break
            }
        }
        flush()
        return ParsedSSHConfig(hosts: hosts, patterns: patterns, hasIncludeOrMatch: hasIncludeOrMatch)
    }
}
