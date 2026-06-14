import Foundation

/// Owns ~/.ssh/ssh2fa.conf (per-registered-host ControlMaster blocks) and the
/// single managed `Include ssh2fa.conf` line in ~/.ssh/config. Pure string
/// transforms (generate / detect / insert) are unit-tested; FS methods take an
/// explicit `dir` so they're temp-dir-tested.
enum SSHConfigManager {
    static let beginMarker = "# >>> SSH2FA managed (Include) >>>"
    static let endMarker   = "# <<< SSH2FA managed (Include) <<<"
    static let includeLine = "Include ssh2fa.conf"

    // MARK: - Pure transforms

    /// The full ssh2fa.conf body for a set of aliases (sorted → stable output).
    /// Per-host ControlPath = the daemon's fallback path so daemon + clients
    /// agree on one socket and enabling the Include never rebuilds a master.
    static func generateManagedConf(aliases: [String], dir: String) -> String {
        let header = "# Managed by SSH2FA — do not edit. Regenerated on host add/remove.\n"
        let blocks = aliases.sorted().map { alias -> String in
            let cp = SSHPaths.controlPathFallback(dir: dir, alias: alias)
            return """
            Host \(alias)
                ControlMaster auto
                ControlPath \(cp)
                ControlPersist yes
            """
        }
        return header + "\n" + blocks.joined(separator: "\n\n") + (blocks.isEmpty ? "" : "\n")
    }

    /// True if the config text already contains an `Include ssh2fa.conf` line
    /// (marked region OR a bare line).
    static func hasInclude(_ configText: String) -> Bool {
        for raw in configText.split(separator: "\n") {
            if raw.trimmingCharacters(in: .whitespaces).lowercased() == includeLine.lowercased() {
                return true
            }
        }
        return false
    }

    /// Idempotently ensure the marked Include region sits at the TOP of the
    /// config. Any pre-existing managed region or bare include line is removed
    /// first, so re-running yields identical output.
    static func ensureInclude(in configText: String) -> String {
        var kept: [String] = []
        var inRegion = false
        for raw in configText.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(raw)
            let t = line.trimmingCharacters(in: .whitespaces)
            if t == beginMarker { inRegion = true; continue }
            if t == endMarker { inRegion = false; continue }
            if inRegion { continue }
            if t.lowercased() == includeLine.lowercased() { continue }
            kept.append(line)
        }
        while kept.first?.trimmingCharacters(in: .whitespaces).isEmpty == true { kept.removeFirst() }
        while kept.last?.trimmingCharacters(in: .whitespaces).isEmpty == true { kept.removeLast() }
        let region = "\(beginMarker)\n\(includeLine)\n\(endMarker)\n"
        if kept.isEmpty { return region }
        return region + "\n" + kept.joined(separator: "\n") + "\n"
    }

    // MARK: - Filesystem (dir-parameterized for testability)

    /// Resolve a symlinked path to its target (so we back up + write THROUGH the
    /// link, never replacing the symlink with a regular file).
    static func realPath(_ path: String) -> String {
        guard let dest = try? FileManager.default.destinationOfSymbolicLink(atPath: path) else {
            return path
        }
        return dest.hasPrefix("/") ? dest
            : (path as NSString).deletingLastPathComponent + "/" + dest
    }

    /// Write ssh2fa.conf for `aliases` into `dir` (perms 600). Idempotent: skips
    /// the write when content is unchanged. Returns true iff a write happened.
    @discardableResult
    static func writeManagedConf(aliases: [String], dir: String) throws -> Bool {
        let path = SSHPaths.managedConfFile(dir: dir)
        let content = generateManagedConf(aliases: aliases, dir: dir)
        if let existing = try? String(contentsOfFile: path, encoding: .utf8), existing == content {
            return false
        }
        try atomicWrite(content, to: path, perms: 0o600)
        return true
    }

    /// Add the Include to ~/.ssh/config in `dir` after backing the file up.
    /// Creates config if missing. Idempotent. `timestamp` is injected so the
    /// backup name is deterministic/testable.
    static func enableInclude(dir: String, timestamp: String) throws {
        let cfgPath = realPath(SSHPaths.configFile(dir: dir))
        let original = (try? String(contentsOfFile: cfgPath, encoding: .utf8)) ?? ""
        if !original.isEmpty {
            try original.write(toFile: SSHPaths.backupFile(dir: dir, timestamp: timestamp),
                               atomically: true, encoding: .utf8)
        }
        try atomicWrite(ensureInclude(in: original), to: cfgPath, perms: 0o600)
    }

    /// Remove the managed Include region (revert) and delete ssh2fa.conf.
    static func disableInclude(dir: String) throws {
        let cfgPath = realPath(SSHPaths.configFile(dir: dir))
        if let original = try? String(contentsOfFile: cfgPath, encoding: .utf8) {
            var kept: [String] = []
            var inRegion = false
            for raw in original.split(separator: "\n", omittingEmptySubsequences: false) {
                let t = raw.trimmingCharacters(in: .whitespaces)
                if t == beginMarker { inRegion = true; continue }
                if t == endMarker { inRegion = false; continue }
                if inRegion { continue }
                if t.lowercased() == includeLine.lowercased() { continue }
                kept.append(String(raw))
            }
            while kept.first?.trimmingCharacters(in: .whitespaces).isEmpty == true { kept.removeFirst() }
            try atomicWrite(kept.joined(separator: "\n") + (kept.isEmpty ? "" : "\n"),
                            to: cfgPath, perms: 0o600)
        }
        try? FileManager.default.removeItem(atPath: SSHPaths.managedConfFile(dir: dir))
    }

    private static func atomicWrite(_ content: String, to path: String, perms: Int) throws {
        let tmp = path + ".ssh2fa-tmp"
        try content.write(toFile: tmp, atomically: false, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: perms], ofItemAtPath: tmp)
        if FileManager.default.fileExists(atPath: path) {
            _ = try FileManager.default.replaceItemAt(URL(fileURLWithPath: path),
                                                      withItemAt: URL(fileURLWithPath: tmp))
        } else {
            try FileManager.default.moveItem(atPath: tmp, toPath: path)
        }
    }
}
