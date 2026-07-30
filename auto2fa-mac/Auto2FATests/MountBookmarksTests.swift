import XCTest

final class MountBookmarksTests: XCTestCase {

    private func bm(_ host: String, _ path: String, _ label: String = "") -> MountBookmark {
        MountBookmark(host: host, remotePath: path, label: label)
    }

    // MARK: - normalize

    func testNormalizeCollapsesSlashesAndTrailingSlash() {
        XCTAssertEqual(MountBookmarks.normalize("  /a//b/c/  "), "/a/b/c")
        XCTAssertEqual(MountBookmarks.normalize("/a/b/"), "/a/b")
        XCTAssertEqual(MountBookmarks.normalize("/"), "/", "root keeps its slash")
    }

    // MARK: - validate

    func testValidAbsolutePathsAreAccepted() {
        XCTAssertNil(MountBookmarks.validate("/scratch/alice/project"))
        XCTAssertNil(MountBookmarks.validate("/"))
    }

    /// `~` is not expanded by the remote side here, so accepting it would create
    /// a bookmark that silently fails to mount later.
    func testTildePathIsRejectedWithAnExplanation() {
        let err = MountBookmarks.validate("~/project")
        XCTAssertNotNil(err)
        XCTAssertTrue(err!.contains("absolute"))
    }

    func testRelativeEmptyAndControlCharPathsAreRejected() {
        XCTAssertNotNil(MountBookmarks.validate("scratch/project"))
        XCTAssertNotNil(MountBookmarks.validate("   "))
        XCTAssertNotNil(MountBookmarks.validate("/a\nProxyCommand evil"))
    }

    // MARK: - upsert / remove

    /// Re-pinning a path must UPDATE it, not create a second identical-looking
    /// menu entry.
    func testUpsertDeduplicatesByNormalizedPath() {
        var list = MountBookmarks.upsert(bm("k6", "/data", "Data"), into: [])
        list = MountBookmarks.upsert(bm("k6", "/data/", "Datasets"), into: list)
        XCTAssertEqual(list.count, 1, "same path must not duplicate")
        XCTAssertEqual(list[0].label, "Datasets", "label must be updated")
        XCTAssertEqual(list[0].remotePath, "/data", "stored normalized")
    }

    func testUpsertKeepsHostsSeparate() {
        var list = MountBookmarks.upsert(bm("k6", "/data"), into: [])
        list = MountBookmarks.upsert(bm("b8", "/data"), into: list)
        XCTAssertEqual(list.count, 2, "same path on a different host is a different pin")
        XCTAssertEqual(MountBookmarks.forHost("k6", in: list).count, 1)
    }

    func testRemoveMatchesRegardlessOfTrailingSlash() {
        let list = MountBookmarks.upsert(bm("k6", "/data"), into: [])
        XCTAssertTrue(MountBookmarks.remove(host: "k6", remotePath: "/data/", from: list).isEmpty)
    }

    // MARK: - displayName

    func testDisplayNameFallsBackToTheFolderName() {
        XCTAssertEqual(bm("k6", "/scratch/alice/project").displayName, "project")
        XCTAssertEqual(bm("k6", "/scratch/alice/project", "Thesis").displayName, "Thesis")
        XCTAssertEqual(bm("k6", "/").displayName, "/", "root has no last component")
    }

    // MARK: - store round-trip

    func testStoreRoundTripsThroughDisk() throws {
        let url = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("mb-\(UUID().uuidString)")
            .appendingPathComponent("mount_bookmarks.json")
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }

        let list = MountBookmarks.upsert(bm("k6", "/data", "Data"), into: [])
        try MountBookmarkStore.save(list, to: url)
        XCTAssertEqual(MountBookmarkStore.load(from: url), list)
    }

    func testMissingOrGarbageFileLoadsEmptyInsteadOfThrowing() throws {
        let dir = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("mb-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }

        let missing = dir.appendingPathComponent("nope.json")
        XCTAssertTrue(MountBookmarkStore.load(from: missing).isEmpty)

        let garbage = dir.appendingPathComponent("garbage.json")
        try "not json".write(to: garbage, atomically: true, encoding: .utf8)
        XCTAssertTrue(MountBookmarkStore.load(from: garbage).isEmpty)
    }
}
