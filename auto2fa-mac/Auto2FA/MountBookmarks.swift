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
    /// Mount this folder automatically as soon as the host connects.
    ///
    /// This is the point of the feature: pinning removes the navigating, and
    /// auto-mount removes the remembering. Only ONE folder per host can be
    /// auto-mounted (there is a single mount point per host), so the first
    /// auto-mount pin wins.
    var autoMount: Bool = false

    // Explicit Decodable: the synthesized one THROWS on a missing key, so
    // adding `autoMount` would make every previously-saved bookmarks file fail
    // to load — silently wiping the user's pins. decodeIfPresent defaults it.
    init(host: String, remotePath: String, label: String, autoMount: Bool = false) {
        self.host = host
        self.remotePath = remotePath
        self.label = label
        self.autoMount = autoMount
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        host = try c.decode(String.self, forKey: .host)
        remotePath = try c.decode(String.self, forKey: .remotePath)
        label = (try? c.decode(String.self, forKey: .label)) ?? ""
        autoMount = (try? c.decodeIfPresent(Bool.self, forKey: .autoMount)) as? Bool ?? false
    }

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
        out.append(MountBookmark(host: bookmark.host, remotePath: path,
                                 label: bookmark.label, autoMount: bookmark.autoMount))
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

    /// Directory name the daemon mounts this path under (`~/Mounts/<host>/<slug>`).
    ///
    /// MUST match `a2fa_core::mounts::slug_for` — the app derives it to tell
    /// which pinned folder is mounted, and a mismatch would render every folder
    /// as unmounted while it is in fact mounted. A parity test on each side
    /// pins the shared cases, including the non-ASCII ones.
    ///
    /// The trailing hash is not decoration: sanitising alone COLLIDES. `/a/b`
    /// and `/a-b` both reduce to `a-b`, and every non-ASCII path reduced to
    /// nothing at all — `/数据`, `/项目` and `/` all produced `root`, so
    /// mounting one shadowed another.
    static func slug(for remotePath: String) -> String {
        let trimmed = remotePath.trimmingCharacters(in: .whitespaces)
            .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        if trimmed.isEmpty { return "root" }
        let allowed = Set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-")
        var base = String(trimmed.map { allowed.contains($0) ? $0 : "-" })
        while base.contains("--") { base = base.replacingOccurrences(of: "--", with: "-") }
        base = base.trimmingCharacters(in: CharacterSet(charactersIn: "-"))
        if base.count > 48 {
            base = String(base.prefix(48)).trimmingCharacters(in: CharacterSet(charactersIn: "-"))
        }
        let h = String(format: "%08x", fnv1a32(trimmed))
        return base.isEmpty ? "path-\(h)" : "\(base)-\(h)"
    }

    /// FNV-1a (32-bit) over UTF-8 bytes — mirrors the Rust `fnv1a32`.
    private static func fnv1a32(_ s: String) -> UInt32 {
        var h: UInt32 = 0x811c9dc5
        for b in Array(s.utf8) {
            h ^= UInt32(b)
            h = h &* 0x01000193
        }
        return h
    }

    /// Should this host be auto-mounted right now?
    ///
    /// Deliberately NOT edge-triggered on "just became ready". That was the
    /// original design and it made the feature almost never fire: the app
    /// assigns `hosts` (running this check) BEFORE it loads the pinned folders,
    /// so on the first poll there were no pins to act on — and by the second
    /// poll the host was already ready, so the edge had passed. Hosts are
    /// normally already connected at launch (the daemon adopts live masters),
    /// so in practice auto-mount only ever fired after a disconnect.
    ///
    /// `alreadyAttempted` is the real idempotence mechanism — the caller clears
    /// it when the host drops, so one attempt happens per connection, and a
    /// manual unmount is respected until the next reconnect.
    static func shouldAutoMount(isReady: Bool,
                                isMounted: Bool,
                                alreadyAttempted: Bool,
                                hasAutoPin: Bool) -> Bool {
        isReady && !isMounted && !alreadyAttempted && hasAutoPin
    }

    /// The folder to mount automatically when `host` connects, if any.
    ///
    /// There is one mount point per host, so at most one pin can win; taking
    /// the first in display order makes the choice predictable rather than
    /// dependent on insertion order.
    static func autoMountPath(for host: String, in list: [MountBookmark]) -> String? {
        forHost(host, in: list).first { $0.autoMount }?.remotePath
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
