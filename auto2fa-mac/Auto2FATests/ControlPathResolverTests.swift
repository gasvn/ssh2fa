import XCTest

final class ControlPathResolverTests: XCTestCase {
    func testPicksExplicitControlPath() {
        let g = "user shgao\ncontrolpath /Users/x/.ssh/cm-ssh2fa-kempner\nport 22\n"
        XCTAssertEqual(ControlPathResolver.pick(fromSSHG: g, alias: "kempner", dir: "/Users/x/.ssh"),
                       "/Users/x/.ssh/cm-ssh2fa-kempner")
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
}
