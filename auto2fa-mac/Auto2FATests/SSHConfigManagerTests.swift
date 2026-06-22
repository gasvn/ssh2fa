import XCTest

final class SSHConfigManagerTests: XCTestCase {
    func testGeneratedConfIsSortedWithCorrectControlPath() {
        let out = SSHConfigManager.generateManagedConf(
            hosts: [.init(alias: "b", conn: nil), .init(alias: "a", conn: nil)], dir: "/d")
        XCTAssertTrue(out.hasPrefix("# Managed by SSH2FA"))
        // sorted: a before b
        let aIdx = out.range(of: "Host a")!.lowerBound
        let bIdx = out.range(of: "Host b")!.lowerBound
        XCTAssertLessThan(aIdx, bIdx)
        XCTAssertTrue(out.contains("ControlPath /d/cm-ssh2fa-a"))
        XCTAssertTrue(out.contains("ControlMaster auto"))
        XCTAssertTrue(out.contains("ControlPersist yes"))
    }

    func testHasIncludeDetectsBareLine() {
        XCTAssertTrue(SSHConfigManager.hasInclude("Host x\nInclude ssh2fa.conf\n"))
        XCTAssertFalse(SSHConfigManager.hasInclude("Host x\n  User u\n"))
    }

    func testEnsureIncludePutsRegionAtTop() {
        let out = SSHConfigManager.ensureInclude(in: "Host kempner\n    User shgao\n")
        XCTAssertTrue(out.hasPrefix(SSHConfigManager.beginMarker))
        XCTAssertTrue(out.contains("Include ssh2fa.conf"))
        XCTAssertTrue(out.contains("Host kempner"))   // user block preserved
    }

    func testEnsureIncludeIsIdempotent() {
        let once = SSHConfigManager.ensureInclude(in: "Host k\n")
        let twice = SSHConfigManager.ensureInclude(in: once)
        XCTAssertEqual(once, twice)
        // exactly one include line
        XCTAssertEqual(twice.components(separatedBy: "Include ssh2fa.conf").count - 1, 1)
    }

    func testEnsureIncludeNormalizesAPreexistingBareInclude() {
        let input = "Include ssh2fa.conf\nHost k\n"
        let out = SSHConfigManager.ensureInclude(in: input)
        XCTAssertEqual(out.components(separatedBy: "Include ssh2fa.conf").count - 1, 1)
        XCTAssertTrue(out.hasPrefix(SSHConfigManager.beginMarker))
    }

    func testEnsureIncludeOnEmptyConfig() {
        let out = SSHConfigManager.ensureInclude(in: "")
        XCTAssertEqual(out, "\(SSHConfigManager.beginMarker)\nInclude ssh2fa.conf\n\(SSHConfigManager.endMarker)\n")
    }

    func testEnsureIncludeIdempotentOnCRLFConfig() {
        // A CRLF config that already has the managed region must be detected
        // (CR trimmed in comparison) and not duplicated.
        let once = SSHConfigManager.ensureInclude(in: "Host k\n")
        let crlf = once.replacingOccurrences(of: "\n", with: "\r\n")
        let again = SSHConfigManager.ensureInclude(in: crlf)
        XCTAssertEqual(again.components(separatedBy: "Include ssh2fa.conf").count - 1, 1)
        XCTAssertTrue(again.hasPrefix(SSHConfigManager.beginMarker))
    }

    private func tempDir() -> String {
        let d = NSTemporaryDirectory() + "ssh2fa-test-" + UUID().uuidString
        try? FileManager.default.createDirectory(atPath: d, withIntermediateDirectories: true)
        return d
    }

    func testWriteManagedConfCreatesFileWithPerms() throws {
        let dir = tempDir()
        let wrote = try SSHConfigManager.writeManagedConf(hosts: [.init(alias: "k", conn: nil)], dir: dir)
        XCTAssertTrue(wrote)
        let path = SSHPaths.managedConfFile(dir: dir)
        XCTAssertTrue(FileManager.default.fileExists(atPath: path))
        let attrs = try FileManager.default.attributesOfItem(atPath: path)
        XCTAssertEqual((attrs[.posixPermissions] as? NSNumber)?.intValue, 0o600)
    }

    func testWriteManagedConfSkipsUnchanged() throws {
        let dir = tempDir()
        XCTAssertTrue(try SSHConfigManager.writeManagedConf(hosts: [.init(alias: "k", conn: nil)], dir: dir))
        XCTAssertFalse(try SSHConfigManager.writeManagedConf(hosts: [.init(alias: "k", conn: nil)], dir: dir))
    }

    func testEnableIncludeBacksUpAndAddsRegion() throws {
        let dir = tempDir()
        let cfg = SSHPaths.configFile(dir: dir)
        try "Host kempner\n    User shgao\n".write(toFile: cfg, atomically: true, encoding: .utf8)
        try SSHConfigManager.enableInclude(dir: dir, timestamp: "TS")
        let after = try String(contentsOfFile: cfg, encoding: .utf8)
        XCTAssertTrue(after.hasPrefix(SSHConfigManager.beginMarker))
        XCTAssertTrue(after.contains("Host kempner"))
        let backup = try String(contentsOfFile: SSHPaths.backupFile(dir: dir, timestamp: "TS"),
                                encoding: .utf8)
        XCTAssertEqual(backup, "Host kempner\n    User shgao\n")
    }

    func testEnableIncludeCreatesMissingConfig() throws {
        let dir = tempDir()
        try SSHConfigManager.enableInclude(dir: dir, timestamp: "TS")
        let after = try String(contentsOfFile: SSHPaths.configFile(dir: dir), encoding: .utf8)
        XCTAssertTrue(after.contains("Include ssh2fa.conf"))
        // No original content → no backup file.
        XCTAssertFalse(FileManager.default.fileExists(
            atPath: SSHPaths.backupFile(dir: dir, timestamp: "TS")))
    }

    func testEnableIncludeTwiceIsStable() throws {
        let dir = tempDir()
        let cfg = SSHPaths.configFile(dir: dir)
        try "Host k\n".write(toFile: cfg, atomically: true, encoding: .utf8)
        try SSHConfigManager.enableInclude(dir: dir, timestamp: "T1")
        let firstPass = try String(contentsOfFile: cfg, encoding: .utf8)
        try SSHConfigManager.enableInclude(dir: dir, timestamp: "T2")
        let secondPass = try String(contentsOfFile: cfg, encoding: .utf8)
        XCTAssertEqual(firstPass, secondPass)
        XCTAssertEqual(secondPass.components(separatedBy: "Include ssh2fa.conf").count - 1, 1)
    }

    func testDisableIncludeRemovesIncludeButKeepsConf() throws {
        let dir = tempDir()
        let cfg = SSHPaths.configFile(dir: dir)
        try "Host k\n".write(toFile: cfg, atomically: true, encoding: .utf8)
        try SSHConfigManager.writeManagedConf(hosts: [.init(alias: "k", conn: nil)], dir: dir)
        try SSHConfigManager.enableInclude(dir: dir, timestamp: "T1")
        try SSHConfigManager.disableInclude(dir: dir)
        let after = try String(contentsOfFile: cfg, encoding: .utf8)
        XCTAssertFalse(after.contains("Include ssh2fa.conf"))
        XCTAssertTrue(after.contains("Host k"))
        // ssh2fa.conf is now load-bearing (the daemon reads it via `ssh -F`), so
        // disabling terminal-reuse must KEEP it — only the user-config Include
        // line is removed. It is owned by AppState.syncManagedSSHConfig.
        XCTAssertTrue(FileManager.default.fileExists(atPath: SSHPaths.managedConfFile(dir: dir)))
    }
}
