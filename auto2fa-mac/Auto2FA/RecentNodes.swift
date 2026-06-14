import Foundation

/// UserDefaults-backed history of compute nodes the user has forwarded to,
/// most-recent-first and capped. Powers the "Recent" quick-pick in the node
/// picker so researchers don't re-hunt the same node every time.
enum RecentNodes {
    static let key = "auto2fa.recentNodes"
    static let cap = 8

    static func record(_ node: String) {
        UserDefaults.standard.set(updated(all(), adding: node, cap: cap), forKey: key)
    }

    static func all() -> [String] {
        (UserDefaults.standard.array(forKey: key) as? [String]) ?? []
    }

    /// Pure: returns `list` with `node` moved/added to the front (deduped),
    /// trimmed, capped to `cap`. A blank node leaves the list unchanged.
    static func updated(_ list: [String], adding node: String, cap: Int) -> [String] {
        let n = node.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !n.isEmpty else { return list }
        var out = list.filter { $0 != n }
        out.insert(n, at: 0)
        return Array(out.prefix(cap))
    }
}
