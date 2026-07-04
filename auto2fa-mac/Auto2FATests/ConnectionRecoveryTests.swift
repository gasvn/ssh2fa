import XCTest

final class ConnectionRecoveryTests: XCTestCase {
    func testSlowBannerShowsOnlyAtOrPastThreshold() {
        let t = ConnectionRecovery.forceReconnectThreshold
        XCTAssertFalse(ConnectionRecovery.shouldShowSlowBanner(failStreak: 0))
        XCTAssertFalse(ConnectionRecovery.shouldShowSlowBanner(failStreak: t - 1))
        XCTAssertTrue(ConnectionRecovery.shouldShowSlowBanner(failStreak: t))
        XCTAssertTrue(ConnectionRecovery.shouldShowSlowBanner(failStreak: t + 5))
    }

    func testForceReconnectFiresExactlyOnceAtTheCrossing() {
        let t = ConnectionRecovery.forceReconnectThreshold
        // Below the threshold: don't touch the socket on a transient blip.
        for s in 0..<t {
            XCTAssertFalse(ConnectionRecovery.shouldForceReconnect(failStreak: s),
                           "streak \(s) must not force a drop")
        }
        // Exactly at the crossing: drop once.
        XCTAssertTrue(ConnectionRecovery.shouldForceReconnect(failStreak: t))
        // Past the crossing: do NOT keep dropping (would cancel the in-flight
        // reconnect every poll).
        for s in (t + 1)...(t + 5) {
            XCTAssertFalse(ConnectionRecovery.shouldForceReconnect(failStreak: s),
                           "streak \(s) must not re-drop the reconnecting socket")
        }
    }

    func testThresholdIsSaneHeartbeatWindow() {
        // 3 consecutive 5s polls ≈ 15s of a dead heartbeat — long enough to be a
        // real drop, short enough to recover quickly.
        XCTAssertGreaterThanOrEqual(ConnectionRecovery.forceReconnectThreshold, 2)
        XCTAssertLessThanOrEqual(ConnectionRecovery.forceReconnectThreshold, 5)
    }
}
