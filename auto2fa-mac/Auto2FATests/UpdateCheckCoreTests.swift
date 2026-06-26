import XCTest

// UpdateCheckCore is compiled into THIS test bundle via project.yml
// (sources include Auto2FA/UpdateCheckCore.swift) — same module, no import.
final class UpdateCheckCoreTests: XCTestCase {

    // MARK: normalizeTag

    func testNormalizeTagStripsLeadingV() {
        XCTAssertEqual(UpdateCheckCore.normalizeTag("v1.2.3"), "1.2.3")
        XCTAssertEqual(UpdateCheckCore.normalizeTag("V2.0"), "2.0")
    }

    func testNormalizeTagLeavesPlainVersion() {
        XCTAssertEqual(UpdateCheckCore.normalizeTag("1.0.0"), "1.0.0")
    }

    func testNormalizeTagTrimsWhitespace() {
        XCTAssertEqual(UpdateCheckCore.normalizeTag("  v1.0  "), "1.0")
    }

    func testNormalizeTagDoesNotStripVNotFollowedByDigit() {
        // A leading "v" that isn't a version prefix (e.g. a word) must survive,
        // so we never mangle a non-version tag into something accidentally
        // parseable.
        XCTAssertEqual(UpdateCheckCore.normalizeTag("vanity"), "vanity")
    }

    // MARK: isNewer

    func testIsNewerComparesNumericallyNotLexically() {
        // The whole reason this isn't a string compare: "10" > "9".
        XCTAssertTrue(UpdateCheckCore.isNewer("1.2.10", than: "1.2.9"))
        XCTAssertFalse(UpdateCheckCore.isNewer("1.2.9", than: "1.2.10"))
    }

    func testIsNewerEqualIsNotNewer() {
        XCTAssertFalse(UpdateCheckCore.isNewer("1.0.0", than: "1.0.0"))
    }

    func testIsNewerTreatsMissingComponentsAsZero() {
        // "1.0" and "1.0.0" are the same release; "1.1" beats "1.0.0".
        XCTAssertFalse(UpdateCheckCore.isNewer("1.0", than: "1.0.0"))
        XCTAssertTrue(UpdateCheckCore.isNewer("1.1", than: "1.0.0"))
    }

    func testIsNewerMajorBeatsMinorAndPatch() {
        XCTAssertTrue(UpdateCheckCore.isNewer("2.0.0", than: "1.9.9"))
    }

    func testIsNewerNonNumericNeverNags() {
        // Garbage tag → treated as 0.0.0 → never "newer" (don't nag on junk).
        XCTAssertFalse(UpdateCheckCore.isNewer("garbage", than: "1.0.0"))
    }

    // MARK: shouldCheckNow (daily-throttle gate)

    func testShouldCheckDisabledNeverChecks() {
        XCTAssertFalse(UpdateCheckCore.shouldCheckNow(
            enabled: false, lastCheck: nil,
            now: Date(timeIntervalSince1970: 1_000_000), interval: 86_400))
    }

    func testShouldCheckNoPriorCheckRunsImmediately() {
        XCTAssertTrue(UpdateCheckCore.shouldCheckNow(
            enabled: true, lastCheck: nil,
            now: Date(timeIntervalSince1970: 1_000_000), interval: 86_400))
    }

    func testShouldCheckWithinIntervalSkips() {
        let now = Date(timeIntervalSince1970: 1_000_000)
        let last = now.addingTimeInterval(-23 * 3_600) // 23h ago
        XCTAssertFalse(UpdateCheckCore.shouldCheckNow(
            enabled: true, lastCheck: last, now: now, interval: 86_400))
    }

    func testShouldCheckPastIntervalRuns() {
        let now = Date(timeIntervalSince1970: 1_000_000)
        let last = now.addingTimeInterval(-25 * 3_600) // 25h ago
        XCTAssertTrue(UpdateCheckCore.shouldCheckNow(
            enabled: true, lastCheck: last, now: now, interval: 86_400))
    }

    func testShouldCheckAtExactIntervalBoundaryRuns() {
        let now = Date(timeIntervalSince1970: 1_000_000)
        let last = now.addingTimeInterval(-86_400) // exactly the interval ago
        XCTAssertTrue(UpdateCheckCore.shouldCheckNow(
            enabled: true, lastCheck: last, now: now, interval: 86_400))
    }

    // MARK: shouldNotify (one reminder per new version)

    func testShouldNotifyFirstTimeForNewerVersion() {
        XCTAssertTrue(UpdateCheckCore.shouldNotify(
            latest: "1.1.0", current: "1.0.0", lastNotified: nil, skipped: nil))
    }

    func testShouldNotNotifyTwiceForSameVersion() {
        XCTAssertFalse(UpdateCheckCore.shouldNotify(
            latest: "1.1.0", current: "1.0.0", lastNotified: "1.1.0", skipped: nil))
    }

    func testShouldNotifyAgainForAnEvenNewerVersion() {
        XCTAssertTrue(UpdateCheckCore.shouldNotify(
            latest: "1.2.0", current: "1.0.0", lastNotified: "1.1.0", skipped: nil))
    }

    func testShouldNotNotifyWhenNotNewer() {
        XCTAssertFalse(UpdateCheckCore.shouldNotify(
            latest: "1.0.0", current: "1.0.0", lastNotified: nil, skipped: nil))
        XCTAssertFalse(UpdateCheckCore.shouldNotify(
            latest: "0.9.0", current: "1.0.0", lastNotified: nil, skipped: nil))
    }

    func testShouldNotNotifyForASkippedVersion() {
        // User chose "Skip this version" → no notification even though it's newer.
        XCTAssertFalse(UpdateCheckCore.shouldNotify(
            latest: "1.1.0", current: "1.0.0", lastNotified: nil, skipped: "1.1.0"))
    }

    func testShouldNotifyForAVersionNewerThanTheSkippedOne() {
        // Skipped 1.1.0, but 1.2.0 is even newer → still notify.
        XCTAssertTrue(UpdateCheckCore.shouldNotify(
            latest: "1.2.0", current: "1.0.0", lastNotified: nil, skipped: "1.1.0"))
    }

    // MARK: shouldSurface (menu-bar marker + About pane)

    func testShouldSurfaceNewerUnskippedVersion() {
        XCTAssertTrue(UpdateCheckCore.shouldSurface(
            latest: "1.1.0", current: "1.0.0", skipped: nil))
    }

    func testShouldNotSurfaceSkippedVersion() {
        XCTAssertFalse(UpdateCheckCore.shouldSurface(
            latest: "1.1.0", current: "1.0.0", skipped: "1.1.0"))
    }

    func testShouldNotSurfaceWhenUpToDate() {
        XCTAssertFalse(UpdateCheckCore.shouldSurface(
            latest: "1.0.0", current: "1.0.0", skipped: nil))
    }

    // MARK: displayVersion (one consistent "vX.Y.Z" format everywhere)

    func testDisplayVersionAddsLeadingV() {
        XCTAssertEqual(UpdateCheckCore.displayVersion("1.2.3"), "v1.2.3")
    }

    func testDisplayVersionKeepsSingleV() {
        XCTAssertEqual(UpdateCheckCore.displayVersion("v1.2.3"), "v1.2.3")
        XCTAssertEqual(UpdateCheckCore.displayVersion("V1.2.3"), "v1.2.3")
    }

    func testDisplayVersionEmptyStaysEmpty() {
        XCTAssertEqual(UpdateCheckCore.displayVersion(""), "")
        XCTAssertEqual(UpdateCheckCore.displayVersion("  "), "")
    }
}
