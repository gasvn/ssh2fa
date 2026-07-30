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

    // MARK: - autoMount

    func testAutoMountPathPicksTheFlaggedPin() {
        var list = MountBookmarks.upsert(bm("k6", "/data", "Data"), into: [])
        list = MountBookmarks.upsert(
            MountBookmark(host: "k6", remotePath: "/work", label: "Work", autoMount: true),
            into: list)
        XCTAssertEqual(MountBookmarks.autoMountPath(for: "k6", in: list), "/work")
        XCTAssertNil(MountBookmarks.autoMountPath(for: "b8", in: list),
                     "another host must not inherit it")
    }

    func testNoAutoMountWhenNothingIsFlagged() {
        let list = MountBookmarks.upsert(bm("k6", "/data"), into: [])
        XCTAssertNil(MountBookmarks.autoMountPath(for: "k6", in: list))
    }

    func testUpsertPreservesTheAutoMountFlag() {
        let list = MountBookmarks.upsert(
            MountBookmark(host: "k6", remotePath: "/work", label: "", autoMount: true), into: [])
        XCTAssertTrue(list[0].autoMount, "re-pinning must not silently clear auto-mount")
    }

    /// REGRESSION: `autoMount` was added after the file format shipped. The
    /// synthesized Decodable THROWS on a missing key, which would make the whole
    /// bookmarks file fail to decode — silently wiping every pin the user had.
    func testBookmarksSavedBeforeAutoMountExistedStillDecode() throws {
        let legacy = #"[{"host":"k6","remotePath":"/data","label":"Data"}]"#.data(using: .utf8)!
        let decoded = try JSONDecoder().decode([MountBookmark].self, from: legacy)
        XCTAssertEqual(decoded.count, 1, "a pre-autoMount file must still load")
        XCTAssertEqual(decoded[0].remotePath, "/data")
        XCTAssertFalse(decoded[0].autoMount, "missing flag defaults to off")
    }

    /// A missing `label` must not break decoding either.
    func testBookmarkWithoutLabelDecodes() throws {
        let data = #"[{"host":"k6","remotePath":"/data"}]"#.data(using: .utf8)!
        let decoded = try JSONDecoder().decode([MountBookmark].self, from: data)
        XCTAssertEqual(decoded[0].displayName, "data")
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
