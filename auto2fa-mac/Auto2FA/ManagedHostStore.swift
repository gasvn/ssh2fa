import Foundation

/// One guided-host connection record. The app is the source of truth for these;
/// the daemon never sees them directly — they are rendered into ssh2fa.conf.
struct ManagedHostConn: Codable, Equatable {
    var alias: String
    var hostName: String
    var user: String
    var port: Int
}

/// Tiny JSON sidecar (alias → connection fields) at ~/.ssh2fa/managed_hosts.json.
/// Pure I/O over an injectable file URL so it unit-tests headlessly.
enum ManagedHostStore {
    /// Decode the sidecar; missing/garbage file → empty (never throws to caller).
    static func load(from url: URL) -> [ManagedHostConn] {
        guard let data = try? Data(contentsOf: url) else { return [] }
        return (try? JSONDecoder().decode([ManagedHostConn].self, from: data)) ?? []
    }

    /// Upsert one record by alias and write back atomically. Returns the new list.
    @discardableResult
    static func upsert(_ conn: ManagedHostConn, in url: URL) throws -> [ManagedHostConn] {
        var list = load(from: url).filter { $0.alias != conn.alias }
        list.append(conn)
        list.sort { $0.alias < $1.alias }
        try write(list, to: url)
        return list
    }

    /// Remove the record for `alias` (no-op if absent). Returns the new list.
    @discardableResult
    static func remove(alias: String, in url: URL) throws -> [ManagedHostConn] {
        let list = load(from: url).filter { $0.alias != alias }
        try write(list, to: url)
        return list
    }

    private static func write(_ list: [ManagedHostConn], to url: URL) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                                                withIntermediateDirectories: true)
        let enc = JSONEncoder()
        enc.outputFormatting = [.prettyPrinted, .sortedKeys]
        try enc.encode(list).write(to: url, options: .atomic)
    }
}
