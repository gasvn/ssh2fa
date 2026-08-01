import XCTest

final class ControlPathResolverTests: XCTestCase {
    func testPicksExplicitControlPath() {
        let g = "user alice\ncontrolpath /Users/x/.ssh/cm-ssh2fa-login01\nport 22\n"
        XCTAssertEqual(ControlPathResolver.pick(fromSSHG: g, alias: "login01", dir: "/Users/x/.ssh"),
                       "/Users/x/.ssh/cm-ssh2fa-login01")
    }

    func testExpandsTilde() {
        let g = "controlpath ~/.ssh/cm-ssh2fa-h\n"
        XCTAssertEqual(ControlPathResolver.pick(fromSSHG: g, alias: "h", dir: "/d"),
                       NSHomeDirectory() + "/.ssh/cm-ssh2fa-h")
    }

    func testNoneFallsBack() {
        let g = "controlpath none\n"
        XCTAssertEqual(ControlPathResolver.pick(fromSSHG: g, alias: "h", dir: "/d"),
                       "/d/cm-ssh2fa-h")
    }

    func testMissingControlPathFallsBack() {
        XCTAssertEqual(ControlPathResolver.pick(fromSSHG: "user u\nport 22\n", alias: "h", dir: "/d"),
                       "/d/cm-ssh2fa-h")
    }

    func testSSHGUsesSameDaemonWrapperAsBackgroundMaster() {
        XCTAssertEqual(
            ControlPathResolver.sshGArguments(alias: "cluster", daemonConfig: "/Users/x/.ssh/ssh2fa-daemon.conf"),
            ["-F", "/Users/x/.ssh/ssh2fa-daemon.conf", "-G", "cluster"])
    }

    func testSSHGCanFallBackBeforeWrapperExists() {
        XCTAssertEqual(ControlPathResolver.sshGArguments(alias: "cluster", daemonConfig: nil),
                       ["-G", "cluster"])
    }
}
