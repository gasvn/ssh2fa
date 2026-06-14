import Foundation

/// One concrete `Host` entry parsed from ~/.ssh/config.
struct ConfigHost: Equatable, Hashable {
    let alias: String
    let hostName: String?
    let user: String?
}

/// Result of parsing ~/.ssh/config (optionally following `Include`): the
/// concrete hosts, the wildcard `Host` patterns we deliberately don't resolve
/// (`Host gpu-*`), and whether our view is incomplete — because the config uses
/// `Match` (conditional, can't evaluate) or references an `Include` we couldn't
/// fully resolve. `incompleteView` keeps the reconciliation warning quiet
/// instead of false-alarming on configs it can't fully see.
struct ParsedSSHConfig: Equatable {
    let hosts: [ConfigHost]
    let patterns: [String]
    let hasMatch: Bool
    let includeUnresolved: Bool

    var incompleteView: Bool { hasMatch || includeUnresolved }

    static let empty = ParsedSSHConfig(hosts: [], patterns: [],
                                       hasMatch: false, includeUnresolved: false)
}

/// Parser for ~/.ssh/config. Concrete top-level `Host` blocks become hosts;
/// wildcard/glob patterns (`Host *`, `Host *.edu`) are recorded as `patterns`
/// (used to suppress false drift warnings) but never imported. Tolerant of
/// comments + indentation + key case + CRLF. `parseConfig(at:)` additionally
/// follows `Include` directives (glob + recurse, with cycle/depth guards).
/// `Match` blocks are flagged, not evaluated. Foundation-only → unit-tested.
enum SSHConfigParser {
    /// Convenience: just the concrete hosts of a single file's text.
    static func parse(_ text: String) -> [ConfigHost] { parseFull(text).hosts }

    /// Parse a single file's TEXT. Does not follow Include — if the text
    /// references one, `includeUnresolved` is true (the view is incomplete).
    static func parseFull(_ text: String) -> ParsedSSHConfig {
        let f = parseOneFile(text)
        return ParsedSSHConfig(hosts: f.hosts, patterns: f.patterns,
                               hasMatch: f.hasMatch,
                               includeUnresolved: !f.includeTargets.isEmpty)
    }

    /// Parse the config file at `path`, FOLLOWING `Include` directives. Relative
    /// includes resolve against `configDir` (the ssh dir), tildes expand, and a
    /// glob's last component is matched against the directory. Guards against
    /// cycles and runaway depth.
    static func parseConfig(at path: String, configDir: String) -> ParsedSSHConfig {
        var visited = Set<String>()
        return walk(path, configDir: configDir, depth: 0, visited: &visited)
    }

    // MARK: - Internals

    private struct FileParse {
        var hosts: [ConfigHost] = []
        var patterns: [String] = []
        var hasMatch = false
        var includeTargets: [String] = []
    }

    private static func parseOneFile(_ text: String) -> FileParse {
        var out = FileParse()
        var current: [String] = []     // concrete aliases on the open Host line
        var hostName: String?
        var user: String?

        func flush() {
            for a in current { out.hosts.append(ConfigHost(alias: a, hostName: hostName, user: user)) }
            current = []; hostName = nil; user = nil
        }

        // Swift treats "\r\n" as ONE Character grapheme, so split(separator:"\n")
        // wouldn't break CRLF lines — normalize first.
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
                        out.patterns.append(tok)
                    } else if tok.hasPrefix("!") {
                        continue
                    } else {
                        current.append(tok)
                    }
                }
            case "hostname":
                if hostName == nil { hostName = values.first }
            case "user":
                if user == nil { user = values.first }
            case "include":
                out.includeTargets.append(contentsOf: values)
            case "match":
                out.hasMatch = true
            default:
                break
            }
        }
        flush()
        return out
    }

    /// Max Include nesting depth — a backstop against pathological/cyclic
    /// configs (ssh itself caps recursion).
    private static let maxDepth = 10

    private static func walk(_ path: String, configDir: String,
                             depth: Int, visited: inout Set<String>) -> ParsedSSHConfig {
        if depth > maxDepth {
            return ParsedSSHConfig(hosts: [], patterns: [], hasMatch: false, includeUnresolved: true)
        }
        let real = (path as NSString).standardizingPath
        if visited.contains(real) { return .empty }   // already parsed → don't double-count
        visited.insert(real)
        guard let text = try? String(contentsOfFile: real, encoding: .utf8) else {
            return ParsedSSHConfig(hosts: [], patterns: [], hasMatch: false, includeUnresolved: true)
        }
        let f = parseOneFile(text)
        var hosts = f.hosts
        var patterns = f.patterns
        var hasMatch = f.hasMatch
        var includeUnresolved = false
        for target in f.includeTargets {
            let matches = resolveInclude(target, configDir: configDir)
            if matches.isEmpty {
                // A glob matching nothing is fine (ssh tolerates it); a concrete
                // path pointing at a missing/unreadable file leaves us blind.
                if !target.contains("*") && !target.contains("?") { includeUnresolved = true }
                continue
            }
            for m in matches {
                let sub = walk(m, configDir: configDir, depth: depth + 1, visited: &visited)
                hosts += sub.hosts
                patterns += sub.patterns
                hasMatch = hasMatch || sub.hasMatch
                includeUnresolved = includeUnresolved || sub.includeUnresolved
            }
        }
        return ParsedSSHConfig(hosts: hosts, patterns: patterns,
                               hasMatch: hasMatch, includeUnresolved: includeUnresolved)
    }

    /// Expand an `Include` target into existing regular-file paths (sorted).
    /// Tilde-expands; resolves relative targets against `configDir`; matches the
    /// last path component as a glob when it contains `*`/`?`.
    static func resolveInclude(_ target: String, configDir: String) -> [String] {
        var p = (target as NSString).expandingTildeInPath
        if !p.hasPrefix("/") { p = configDir + "/" + p }
        let dir = (p as NSString).deletingLastPathComponent
        let comp = (p as NSString).lastPathComponent
        if comp.contains("*") || comp.contains("?") {
            let entries = (try? FileManager.default.contentsOfDirectory(atPath: dir)) ?? []
            return entries
                .filter { !$0.hasPrefix(".") && SSHSyncDiff.globMatches(pattern: comp, name: $0) }
                .map { dir + "/" + $0 }
                .filter { isRegularFile($0) }
                .sorted()
        }
        return isRegularFile(p) ? [p] : []
    }

    private static func isRegularFile(_ path: String) -> Bool {
        var isDir: ObjCBool = false
        return FileManager.default.fileExists(atPath: path, isDirectory: &isDir) && !isDir.boolValue
    }
}
