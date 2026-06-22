import XCTest

final class SSHConfigGenTests: XCTestCase {
    private let dir = "/Users/x/.ssh"

    func testManagedBlockWithConnEmitsHostNameUserPort() {
        let conf = SSHConfigManager.generateManagedConf(
            hosts: [.init(alias: "cannon", conn: .init(hostName: "login.example.edu", user: "jdoe", port: 2222))],
            dir: dir)
        XCTAssertTrue(conf.contains("Host cannon"))
        XCTAssertTrue(conf.contains("HostName login.example.edu"))
        XCTAssertTrue(conf.contains("User jdoe"))
        XCTAssertTrue(conf.contains("Port 2222"))
        XCTAssertTrue(conf.contains("ControlMaster auto"))
        XCTAssertFalse(conf.contains("Include"))
    }

    func testManagedBlockWithoutConnIsControlMasterOnly() {
        let conf = SSHConfigManager.generateManagedConf(
            hosts: [.init(alias: "legacy", conn: nil)], dir: dir)
        XCTAssertTrue(conf.contains("Host legacy"))
        XCTAssertTrue(conf.contains("ControlMaster auto"))
        XCTAssertFalse(conf.contains("HostName"))
        XCTAssertFalse(conf.contains("User "))
    }

    func testPort22IsOmitted() {
        let conf = SSHConfigManager.generateManagedConf(
            hosts: [.init(alias: "h", conn: .init(hostName: "a", user: "u", port: 22))], dir: dir)
        XCTAssertFalse(conf.contains("Port 22"))
    }

    func testDaemonWrapperIncludesManagedThenUserConfig() {
        let w = SSHConfigManager.daemonWrapperContent(dir: dir)
        let mIdx = w.range(of: "Include \(dir)/ssh2fa.conf")
        let uIdx = w.range(of: "Include \(dir)/config")
        XCTAssertNotNil(mIdx); XCTAssertNotNil(uIdx)
        XCTAssertTrue(mIdx!.lowerBound < uIdx!.lowerBound, "managed hosts must come before user config")
    }

    func testSanitizeAlias() {
        XCTAssertEqual(SSHConfigManager.sanitizeAlias("My Lab Server!"), "My-Lab-Server")
        XCTAssertEqual(SSHConfigManager.sanitizeAlias("login.rc.fas.harvard.edu"), "login.rc.fas.harvard.edu")
        XCTAssertEqual(SSHConfigManager.sanitizeAlias("  a  b  "), "a-b")
    }

    func testConflictDetection() {
        XCTAssertTrue(SSHConfigManager.aliasConflicts("cannon", userAliases: ["cannon", "other"]))
        XCTAssertFalse(SSHConfigManager.aliasConflicts("fresh", userAliases: ["cannon"]))
    }

    /// ssh Host matching is case-insensitive — the conflict check must be too.
    func testConflictDetectionCaseInsensitive() {
        XCTAssertTrue(SSHConfigManager.aliasConflicts("Cannon", userAliases: ["cannon"]))
        XCTAssertTrue(SSHConfigManager.aliasConflicts("cannon", userAliases: ["CANNON"]))
    }

    /// A newline in HostName/User must not inject a second config DIRECTIVE line
    /// (the value substring staying on the HostName/User line is harmless — ssh
    /// just gets a bogus value; the attack is a new directive on its own line).
    func testHostNameUserNewlineStripped() {
        let conf = SSHConfigManager.generateManagedConf(
            hosts: [.init(alias: "h",
                          conn: .init(hostName: "good\nProxyCommand evil",
                                      user: "u\nIdentityFile /tmp/x", port: 22))],
            dir: "/Users/x/.ssh")
        let directiveLines = conf.split(separator: "\n").map {
            $0.trimmingCharacters(in: .whitespaces)
        }
        XCTAssertFalse(directiveLines.contains { $0.hasPrefix("ProxyCommand") },
                       "no injected ProxyCommand directive line: \(conf)")
        XCTAssertFalse(directiveLines.contains { $0.hasPrefix("IdentityFile") },
                       "no injected IdentityFile directive line: \(conf)")
    }
}
