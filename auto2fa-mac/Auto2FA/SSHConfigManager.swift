import Foundation

/// Owns ~/.ssh/ssh2fa.conf (per-registered-host ControlMaster blocks) and the
/// single managed `Include ssh2fa.conf` line in ~/.ssh/config. Pure string
/// transforms (generate / detect / insert) are unit-tested; FS methods take an
/// explicit `dir` so they're temp-dir-tested.
enum SSHConfigManager {
    static let beginMarker = "# >>> SSH2FA managed (Include) >>>"
    static let endMarker   = "# <<< SSH2FA managed (Include) <<<"
    static let includeLine = "Include ssh2fa.conf"

    /// Normalize line endings to LF before splitting — Swift treats "\r\n" as
    /// ONE Character grapheme, so split(separator: "\n") wouldn't break CRLF
    /// lines and marker detection would miss (→ duplicate Include). We rewrite
    /// the whole config anyway (with a backup), so emitting LF is fine.
    private static func lf(_ s: String) -> String {
        s.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
    }

    // MARK: - Pure transforms (guided-host APIs)

    /// A host to render into the managed conf. `conn == nil` → a legacy/imported
    /// alias that relies on the user's own config; emit only the ControlMaster
    /// block (today's behavior). `conn != nil` → a guided host; emit the full
    /// connection definition so it resolves with no user-config entry.
    struct ManagedHost {
        var alias: String
        var conn: Conn?
        struct Conn { var hostName: String; var user: String; var port: Int }
    }

    static func generateManagedConf(hosts: [ManagedHost], dir: String) -> String {
        let header = "# Managed by SSH2FA — do not edit. Regenerated on host add/remove.\n"
        let blocks = hosts.sorted { $0.alias < $1.alias }.map { h -> String in
            let cp = SSHPaths.controlPathFallback(dir: dir, alias: h.alias)
            var lines = ["Host \(h.alias)"]
            if let c = h.conn {
                // Defensive: strip any newline so a value can never inject a
                // second config directive into the file (HostName/User come
                // from user input). `port` is an Int, inherently safe.
                lines.append("    HostName \(oneLine(c.hostName))")
                lines.append("    User \(oneLine(c.user))")
                if c.port != 22 { lines.append("    Port \(c.port)") }
            }
            lines.append("    ControlMaster auto")
            lines.append("    ControlPath \(cp)")
            lines.append("    ControlPersist yes")
            return lines.joined(separator: "\n")
        }
        return header + "\n" + blocks.joined(separator: "\n\n") + (blocks.isEmpty ? "" : "\n")
    }

    /// Collapse any CR/LF in a config value to nothing — a newline would inject
    /// a second directive line into the generated config.
    private static func oneLine(_ s: String) -> String {
        s.replacingOccurrences(of: "\r", with: "").replacingOccurrences(of: "\n", with: "")
    }

    /// The daemon wrapper (~/.ssh/ssh2fa-daemon.conf) that `ssh -F` reads:
    /// our managed hosts FIRST (so their values win), then the user's config to
    /// inherit globals + legacy hosts. The managed file has no includes, so this
    /// one-directional include chain can never loop.
    static func daemonWrapperContent(dir: String) -> String {
        """
        # Managed by SSH2FA — the daemon reads this via `ssh -F`. Do not edit.
        Include \(dir)/ssh2fa.conf
        Include \(dir)/config
        """ + "\n"
    }

    /// Reduce a user-facing name to a legal ssh `Host` token: trim, collapse
    /// whitespace runs to a single `-`, drop characters ssh treats specially.
    static func sanitizeAlias(_ raw: String) -> String {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        let collapsed = trimmed.replacingOccurrences(of: "\\s+", with: "-",
                                                     options: .regularExpression)
        let allowed = collapsed.unicodeScalars.filter {
            CharacterSet.alphanumerics.contains($0) || "-._".unicodeScalars.contains($0)
        }
        return String(String.UnicodeScalarView(allowed))
    }

    /// True iff `alias` is already a Host the USER defined in their own config.
    /// Case-insensitive (ssh `Host` matching ignores case). Callers should pass
    /// the user's config aliases MINUS the app's own managed aliases — once the
    /// terminal-reuse Include is on, the managed aliases surface in the parsed
    /// config and would otherwise self-conflict on a legitimate re-add/edit.
    static func aliasConflicts(_ alias: String, userAliases: [String]) -> Bool {
        let a = alias.lowercased()
        return userAliases.contains { $0.lowercased() == a }
    }

    // MARK: - Include management

    /// True if the config text already contains an `Include ssh2fa.conf` line
    /// (marked region OR a bare line).
    static func hasInclude(_ configText: String) -> Bool {
        for raw in lf(configText).split(separator: "\n") {
            if raw.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() == includeLine.lowercased() {
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
        for raw in lf(configText).split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(raw)
            let t = line.trimmingCharacters(in: .whitespacesAndNewlines)
            if t == beginMarker { inRegion = true; continue }
            if t == endMarker { inRegion = false; continue }
            if inRegion { continue }
            if t.lowercased() == includeLine.lowercased() { continue }
            kept.append(line)
        }
        while kept.first?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == true { kept.removeFirst() }
        while kept.last?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == true { kept.removeLast() }
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

    /// Write ssh2fa.conf from the host list into `dir` (perms 600). Idempotent:
    /// skips the write when content is unchanged. Returns true iff a write
    /// happened.
    @discardableResult
    static func writeManagedConf(hosts: [ManagedHost], dir: String) throws -> Bool {
        let path = SSHPaths.managedConfFile(dir: dir)
        let content = generateManagedConf(hosts: hosts, dir: dir)
        return try writeIfChanged(content, to: path, perms: 0o600)
    }

    /// Write ~/.ssh/ssh2fa-daemon.conf (the `-F` wrapper) into `dir` (perms 600).
    /// Idempotent. Returns true iff a write happened.
    @discardableResult
    static func writeDaemonWrapper(dir: String) throws -> Bool {
        let path = (dir as NSString).appendingPathComponent("ssh2fa-daemon.conf")
        return try writeIfChanged(daemonWrapperContent(dir: dir), to: path, perms: 0o600)
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
            for raw in lf(original).split(separator: "\n", omittingEmptySubsequences: false) {
                let t = raw.trimmingCharacters(in: .whitespacesAndNewlines)
                if t == beginMarker { inRegion = true; continue }
                if t == endMarker { inRegion = false; continue }
                if inRegion { continue }
                if t.lowercased() == includeLine.lowercased() { continue }
                kept.append(String(raw))
            }
            while kept.first?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == true { kept.removeFirst() }
            try atomicWrite(kept.joined(separator: "\n") + (kept.isEmpty ? "" : "\n"),
                            to: cfgPath, perms: 0o600)
        }
        // Do NOT delete ssh2fa.conf — it is now load-bearing: the daemon reads it
        // via `ssh -F` (through the wrapper). Disabling terminal-reuse only
        // removes the user-config Include line above; the managed conf stays,
        // owned by AppState.syncManagedSSHConfig.
    }

    /// Atomic write (perms) that skips when `content` already matches the file
    /// byte-for-byte. Returns true iff a write actually happened.
    @discardableResult
    private static func writeIfChanged(_ content: String, to path: String, perms: Int) throws -> Bool {
        if let existing = try? String(contentsOfFile: path, encoding: .utf8), existing == content {
            return false
        }
        try atomicWrite(content, to: path, perms: perms)
        return true
    }

    private static func atomicWrite(_ content: String, to path: String, perms: Int) throws {
        let tmp = path + ".ssh2fa-tmp"
        // Never strand a partial temp file: on a mid-write failure the deferred
        // remove cleans it up; on success the move/replace already consumed it
        // so the remove is a harmless no-op.
        defer { try? FileManager.default.removeItem(atPath: tmp) }
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
