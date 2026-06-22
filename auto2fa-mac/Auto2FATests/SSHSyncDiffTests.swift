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
        XCTAssertEqual(SSHSyncDiff.unreachable(registered: ["a", "b"], configAliases: ["a"],
                                               patterns: [], configIncompleteView: false),
                       ["b"])
    }

    func testUnreachableEmptyWhenAllPresent() {
        XCTAssertEqual(SSHSyncDiff.unreachable(registered: ["a"], configAliases: ["a", "z"],
                                               patterns: [], configIncompleteView: false), [])
    }

    func testUnreachableSuppressedWhenConfigHasIncludeOrMatch() {
        // We can't see Included/Matched hosts → never false-alarm.
        XCTAssertEqual(SSHSyncDiff.unreachable(registered: ["a", "b"], configAliases: [],
                                               patterns: [], configIncompleteView: true), [])
    }

    func testUnreachableSuppressedForWildcardCoveredHost() {
        // gpu-04 isn't a literal Host, but `Host gpu-*` covers it → reachable.
        XCTAssertEqual(SSHSyncDiff.unreachable(registered: ["gpu-04", "lonely"],
                                               configAliases: [],
                                               patterns: ["gpu-*"],
                                               configIncompleteView: false),
                       ["lonely"])
    }

    func testGlobMatches() {
        XCTAssertTrue(SSHSyncDiff.globMatches(pattern: "gpu-*", name: "gpu-04"))
        XCTAssertTrue(SSHSyncDiff.globMatches(pattern: "*", name: "anything"))
        XCTAssertTrue(SSHSyncDiff.globMatches(pattern: "node?", name: "node7"))
        XCTAssertTrue(SSHSyncDiff.globMatches(pattern: "*.rc.edu", name: "login.rc.edu"))
        XCTAssertFalse(SSHSyncDiff.globMatches(pattern: "gpu-*", name: "cpu-04"))
        XCTAssertFalse(SSHSyncDiff.globMatches(pattern: "node?", name: "node12"))
        XCTAssertFalse(SSHSyncDiff.globMatches(pattern: "exact", name: "exacts"))
    }
}
