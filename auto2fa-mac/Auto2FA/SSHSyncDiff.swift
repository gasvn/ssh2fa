import Foundation

/// Pure set-diff between the user's ssh config and SSH2FA's registered hosts.
/// Drives the import list (capability 1) and the reconciliation warning
/// (capability 4). Foundation-only → unit-tested.
enum SSHSyncDiff {
    /// Config hosts not yet registered (by alias), preserving config order,
    /// deduped by alias.
    static func importable(configHosts: [ConfigHost], registered: [String]) -> [ConfigHost] {
        let have = Set(registered)
        var seen = Set<String>()
        var out: [ConfigHost] = []
        for h in configHosts where !have.contains(h.alias) && !seen.contains(h.alias) {
            seen.insert(h.alias); out.append(h)
        }
        return out
    }

    /// Registered aliases that genuinely can't be reached from the config — i.e.
    /// the host won't connect, so the row should warn.
    ///
    /// Deliberately conservative to avoid false alarms:
    /// - If the config uses `Include`/`Match`, our top-level parse can't see the
    ///   whole picture → warn about nothing.
    /// - An alias covered by a wildcard `Host` block (`Host gpu-*` for
    ///   `gpu-04`) IS reachable → not flagged.
    static func unreachable(registered: [String],
                            configAliases: [String],
                            patterns: [String],
                            configHasIncludeOrMatch: Bool) -> [String] {
        if configHasIncludeOrMatch { return [] }
        let cfg = Set(configAliases)
        return registered.filter { alias in
            if cfg.contains(alias) { return false }
            if patterns.contains(where: { globMatches(pattern: $0, name: alias) }) { return false }
            return true
        }
    }

    /// Minimal ssh-style glob match: `*` matches any run (including empty),
    /// `?` matches exactly one character. Classic linear two-pointer algorithm.
    static func globMatches(pattern: String, name: String) -> Bool {
        let p = Array(pattern), s = Array(name)
        var i = 0, j = 0
        var star = -1, mark = 0
        while i < s.count {
            if j < p.count && (p[j] == "?" || p[j] == s[i]) {
                i += 1; j += 1
            } else if j < p.count && p[j] == "*" {
                star = j; mark = i; j += 1
            } else if star != -1 {
                j = star + 1; mark += 1; i = mark
            } else {
                return false
            }
        }
        while j < p.count && p[j] == "*" { j += 1 }
        return j == p.count
    }
}
