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

    /// Registered aliases that no longer appear as a Host in config — these
    /// cannot connect.
    static func unreachable(registered: [String], configAliases: [String]) -> [String] {
        let cfg = Set(configAliases)
        return registered.filter { !cfg.contains($0) }
    }
}
