import XCTest

final class SSHPathsTests: XCTestCase {
    func testDefaultDirIsHomeSSH() {
        XCTAssertEqual(SSHPaths.sshDir(env: [:], home: "/Users/x"), "/Users/x/.ssh")
    }

    func testEnvOverrideStripsTrailingSlash() {
        XCTAssertEqual(SSHPaths.sshDir(env: ["SSH_CONFIG_PATH": "/Users/x/.ssh/"],
                                       home: "/Users/x"), "/Users/x/.ssh")
    }

    func testEnvTildeExpansion() {
        let dir = SSHPaths.sshDir(env: ["SSH_CONFIG_PATH": "~/alt"], home: NSHomeDirectory())
        XCTAssertEqual(dir, NSHomeDirectory() + "/alt")
    }

    func testFilePaths() {
        XCTAssertEqual(SSHPaths.configFile(dir: "/d"), "/d/config")
        XCTAssertEqual(SSHPaths.managedConfFile(dir: "/d"), "/d/ssh2fa.conf")
        XCTAssertEqual(SSHPaths.backupFile(dir: "/d", timestamp: "20260613-120000"),
                       "/d/config.ssh2fa-backup-20260613-120000")
    }

    func testControlPathFallback() {
        XCTAssertEqual(SSHPaths.controlPathFallback(dir: "/Users/x/.ssh", alias: "login01"),
                       "/Users/x/.ssh/cm-ssh2fa-login01")
    }
}
