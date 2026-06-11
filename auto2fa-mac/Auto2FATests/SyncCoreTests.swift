import XCTest

// LockCore / SyncCore / SyncPayload are compiled into THIS test bundle via
// project.yml (sources include Auto2FA/SyncCore.swift) — same module, no import.
final class SyncCoreTests: XCTestCase {
    // MARK: LockCore.shouldChallenge
    func testLockDisabledNeverChallenges() {
        XCTAssertFalse(LockCore.shouldChallenge(enabled: false, lastAuth: nil,
            now: Date(), grace: 60))
    }
    func testLockNoPriorAuthChallenges() {
        XCTAssertTrue(LockCore.shouldChallenge(enabled: true, lastAuth: nil,
            now: Date(), grace: 60))
    }
    func testLockWithinGraceDoesNotChallenge() {
        let now = Date(timeIntervalSince1970: 1000)
        let last = Date(timeIntervalSince1970: 970)   // 30s ago, grace 60
        XCTAssertFalse(LockCore.shouldChallenge(enabled: true, lastAuth: last,
            now: now, grace: 60))
    }
    func testLockPastGraceChallenges() {
        let now = Date(timeIntervalSince1970: 1000)
        let last = Date(timeIntervalSince1970: 930)   // 70s ago, grace 60
        XCTAssertTrue(LockCore.shouldChallenge(enabled: true, lastAuth: last,
            now: now, grace: 60))
    }

    // MARK: SyncCore.resolve
    func testResolveNoRemoteWritesLocal() {
        XCTAssertEqual(SyncCore.resolve(remoteUpdatedAt: nil,
            lastAppliedRemoteAt: 0, localLastWriteAt: 0), .writeLocal)
    }
    func testResolveRemoteNewerApplies() {
        XCTAssertEqual(SyncCore.resolve(remoteUpdatedAt: 200,
            lastAppliedRemoteAt: 100, localLastWriteAt: 100), .applyRemote)
    }
    func testResolveLocalNewerWrites() {
        XCTAssertEqual(SyncCore.resolve(remoteUpdatedAt: 100,
            lastAppliedRemoteAt: 100, localLastWriteAt: 200), .writeLocal)
    }
    func testResolveAlreadyAppliedNoop() {
        XCTAssertEqual(SyncCore.resolve(remoteUpdatedAt: 100,
            lastAppliedRemoteAt: 100, localLastWriteAt: 100), .noop)
    }

    // MARK: SyncPayload round-trip
    func testPayloadRoundTrip() throws {
        let p = SyncPayload(version: 1, updatedAt: 12345.0,
            values: ["a": true, "b": false])
        let data = try JSONEncoder().encode(p)
        let back = try JSONDecoder().decode(SyncPayload.self, from: data)
        XCTAssertEqual(p, back)
    }
}
