import Foundation

/// One pinned remote folder for a host — a place you actually work, saved so
/// you never navigate to it again.
struct MountBookmark: Codable, Equatable, Identifiable, Hashable {
    /// Stable id so SwiftUI lists don't reorder on rename.
    var id: String { "\(host)\u{0}\(remotePath)" }
    var host: String
    /// Absolute remote directory, e.g. `/scratch/alice/project`.
    var remotePath: String
    /// What the user calls it. Empty → the UI falls back to the path's last
    /// component.
    var label: String

    /// Display name: the label if given, else the trailing folder name, else
    /// the path itself (for "/").
    var displayName: String {
        let l = label.trimmingCharacters(in: .whitespacesAndNewlines)
        if !l.isEmpty { return l }
        let last = remotePath.split(separator: "/").last.map(String.init)
        return last ?? remotePath
    }
}

/// Pure logic for pinned mount folders: normalization, validation, ordering.
/// Foundation-only so it unit-tests headlessly.
enum MountBookmarks {
    /// Canonical form of a user-typed remote path.
    ///
    /// Trims whitespace, collapses duplicate slashes, drops a trailing slash
    /// (except for root). `~` is expanded to nothing useful remotely, so a
    /// leading `~/` is rewritten as-is — sshfs resolves it relative to the
    /// remote home only when unquoted, which we cannot rely on, so we require
    /// absolute paths and say so in `validate`.
    static func normalize(_ raw: String) -> String {
        var p = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        while p.contains("//") { p = p.replacingOccurrences(of: "//", with: "/") }
        if p.count > 1 && p.hasSuffix("/") { p.removeLast() }
        return p
    }

    /// Why this path can't be pinned, or nil if it's fine.
    static func validate(_ raw: String) -> String? {
        let p = normalize(raw)
        if p.isEmpty { return "Enter a folder path." }
        if p.hasPrefix("~") {
            return "Use an absolute path (e.g. /home/you/project) — ~ isn't expanded here."
        }
        if !p.hasPrefix("/") { return "The path must start with “/”." }
        if p.unicodeScalars.contains(where: { $0.value < 0x20 }) {
            return "The path can't contain line breaks."
        }
        return nil
    }

    /// Insert or update a bookmark, keyed by (host, normalized path), and keep
    /// the list sorted for a stable menu order.
    ///
    /// Re-pinning a path you already pinned updates its label rather than
    /// creating a duplicate entry that renders as two identical menu items.
    static func upsert(_ bookmark: MountBookmark, into list: [MountBookmark]) -> [MountBookmark] {
        let path = normalize(bookmark.remotePath)
        var out = list.filter { !($0.host == bookmark.host && normalize($0.remotePath) == path) }
        out.append(MountBookmark(host: bookmark.host, remotePath: path, label: bookmark.label))
        return sorted(out)
    }

    static func remove(host: String, remotePath: String, from list: [MountBookmark]) -> [MountBookmark] {
        let path = normalize(remotePath)
        return sorted(list.filter { !($0.host == host && normalize($0.remotePath) == path) })
    }

    /// Bookmarks for one host, in display order.
    static func forHost(_ host: String, in list: [MountBookmark]) -> [MountBookmark] {
        sorted(list.filter { $0.host == host })
    }

    private static func sorted(_ list: [MountBookmark]) -> [MountBookmark] {
        list.sorted {
            $0.host == $1.host
                ? $0.displayName.localizedStandardCompare($1.displayName) == .orderedAscending
                : $0.host < $1.host
        }
    }
}

/// JSON sidecar at ~/.ssh2fa/mount_bookmarks.json. Mirrors `ManagedHostStore`:
/// pure I/O over an injectable URL so it unit-tests headlessly.
enum MountBookmarkStore {
    static func load(from url: URL) -> [MountBookmark] {
        guard let data = try? Data(contentsOf: url) else { return [] }
        return (try? JSONDecoder().decode([MountBookmark].self, from: data)) ?? []
    }

    @discardableResult
    static func save(_ list: [MountBookmark], to url: URL) throws -> [MountBookmark] {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                                                withIntermediateDirectories: true)
        let enc = JSONEncoder()
        enc.outputFormatting = [.prettyPrinted, .sortedKeys]
        try enc.encode(list).write(to: url, options: .atomic)
        // Contents are paths, not secrets, but keep it owner-only like the
        // other app-managed sidecars.
        try? FileManager.default.setAttributes([.posixPermissions: 0o600],
                                               ofItemAtPath: url.path)
        return list
    }
}
