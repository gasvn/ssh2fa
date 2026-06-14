import XCTest

final class SSHConfigManagerTests: XCTestCase {
    func testGeneratedConfIsSortedWithCorrectControlPath() {
        let out = SSHConfigManager.generateManagedConf(aliases: ["b", "a"], dir: "/d")
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
}
