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

    // MARK: - slug parity with the daemon

    /// The app derives the mount directory name to tell which pin is mounted.
    /// It MUST agree with `a2fa_core::mounts::slug_for` — a mismatch would show
    /// every folder as unmounted while it is in fact mounted. These exact values
    /// are pinned on BOTH sides.
    func testSlugMatchesTheDaemonExactly() {
        XCTAssertEqual(MountBookmarks.slug(for: "/"), "root")
        XCTAssertEqual(MountBookmarks.slug(for: ""), "root")
        XCTAssertEqual(MountBookmarks.slug(for: "/scratch"), "scratch-c462a115")
        XCTAssertEqual(MountBookmarks.slug(for: "/scratch/alice/project"),
                       "scratch-alice-project-209c2eb8")
        // Trailing slash is the same path.
        XCTAssertEqual(MountBookmarks.slug(for: "/scratch/alice/project/"),
                       MountBookmarks.slug(for: "/scratch/alice/project"))
    }

    /// REGRESSION (mount-point collision): non-ASCII paths all reduced to
    /// "root", so 数据 / 项目 / "/" shared one mount point and mounting the
    /// second shadowed the first.
    func testNonAsciiPathsGetDistinctSlugs() {
        XCTAssertEqual(MountBookmarks.slug(for: "/数据"), "path-87e94f15")
        XCTAssertEqual(MountBookmarks.slug(for: "/项目"), "path-949087d6")
        XCTAssertNotEqual(MountBookmarks.slug(for: "/数据"), MountBookmarks.slug(for: "/项目"))
        XCTAssertNotEqual(MountBookmarks.slug(for: "/数据"), MountBookmarks.slug(for: "/"))
    }

    /// The other lossy case: a separator and a literal dash.
    func testSeparatorAndDashPathsAreDistinguished() {
        XCTAssertEqual(MountBookmarks.slug(for: "/a/b"), "a-b-3a8e75c1")
        XCTAssertEqual(MountBookmarks.slug(for: "/a-b"), "a-b-2a89df63")
        XCTAssertNotEqual(MountBookmarks.slug(for: "/a/b"), MountBookmarks.slug(for: "/a-b"))
    }

    /// A slug is ONE filesystem component — it must never reintroduce a
    /// separator, or a mount would land outside its host directory.
    func testSlugIsAlwaysASingleComponent() {
        for p in ["/a b/c:d", "/../../etc/passwd", "/x/y/z", "/数据/子目录"] {
            let s = MountBookmarks.slug(for: p)
            XCTAssertFalse(s.contains("/"), "\(p) produced \(s)")
            XCTAssertFalse(s.hasPrefix("-"))
            XCTAssertFalse(s.hasSuffix("-"))
        }
    }

    func testSlugIsLengthCapped() {
        XCTAssertLessThanOrEqual(MountBookmarks.slug(for: "/" + String(repeating: "x", count: 500)).count, 60)
    }

    // MARK: - shouldAutoMount

    func testAutoMountsAReadyUnmountedHostWithAnAutoPin() {
        XCTAssertTrue(MountBookmarks.shouldAutoMount(
            isReady: true, isMounted: false, alreadyAttempted: false, hasAutoPin: true))
    }

    /// REGRESSION: this was edge-triggered on "just became ready", so it almost
    /// never fired — the app loads hosts BEFORE the pinned folders, so the first
    /// poll had no pins, and by the second poll the host was already ready and
    /// the edge had passed. Hosts are normally already connected at launch.
    func testAutoMountsAHostThatWasAlreadyReadyAtLaunch() {
        // No "edge" anywhere in the inputs — readiness alone is enough.
        XCTAssertTrue(MountBookmarks.shouldAutoMount(
            isReady: true, isMounted: false, alreadyAttempted: false, hasAutoPin: true),
            "a host already connected at launch must still auto-mount")
    }

    func testDoesNotAutoMountWhenAlreadyMounted() {
        XCTAssertFalse(MountBookmarks.shouldAutoMount(
            isReady: true, isMounted: true, alreadyAttempted: false, hasAutoPin: true))
    }

    /// One attempt per connection: a manual unmount must stay unmounted rather
    /// than being immediately re-mounted on the next 5s poll.
    func testDoesNotRemountAfterAManualUnmount() {
        XCTAssertFalse(MountBookmarks.shouldAutoMount(
            isReady: true, isMounted: false, alreadyAttempted: true, hasAutoPin: true))
    }

    func testDoesNotAutoMountWithoutAnAutoPinOrWhenNotReady() {
        XCTAssertFalse(MountBookmarks.shouldAutoMount(
            isReady: true, isMounted: false, alreadyAttempted: false, hasAutoPin: false))
        XCTAssertFalse(MountBookmarks.shouldAutoMount(
            isReady: false, isMounted: false, alreadyAttempted: false, hasAutoPin: true))
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
