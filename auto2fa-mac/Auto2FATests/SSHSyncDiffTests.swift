import XCTest

final class SSHSyncDiffTests: XCTestCase {
    private func host(_ a: String) -> ConfigHost { ConfigHost(alias: a, hostName: nil, user: nil) }

    func testImportableExcludesRegisteredPreservesOrder() {
        let cfg = [host("a"), host("b"), host("c")]
        XCTAssertEqual(SSHSyncDiff.importable(configHosts: cfg, registered: ["b"]),
                       [host("a"), host("c")])
    }

    func testImportableDedupesByAlias() {
        let cfg = [host("a"), host("a"), host("b")]
        XCTAssertEqual(SSHSyncDiff.importable(configHosts: cfg, registered: []),
                       [host("a"), host("b")])
    }

    func testImportableEmptyWhenAllRegistered() {
        XCTAssertEqual(SSHSyncDiff.importable(configHosts: [host("a")], registered: ["a"]), [])
    }

    func testUnreachableFindsRegisteredMissingFromConfig() {
        XCTAssertEqual(SSHSyncDiff.unreachable(registered: ["a", "b"], configAliases: ["a"]),
                       ["b"])
    }

    func testUnreachableEmptyWhenAllPresent() {
        XCTAssertEqual(SSHSyncDiff.unreachable(registered: ["a"], configAliases: ["a", "z"]), [])
    }
}
