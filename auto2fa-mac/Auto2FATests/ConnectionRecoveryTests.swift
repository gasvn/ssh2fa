import XCTest

final class ConnectionRecoveryTests: XCTestCase {
    func testSlowBannerShowsOnlyAtOrPastThreshold() {
        let t = ConnectionRecovery.forceReconnectThreshold
        XCTAssertFalse(ConnectionRecovery.shouldShowSlowBanner(failStreak: 0))
        XCTAssertFalse(ConnectionRecovery.shouldShowSlowBanner(failStreak: t - 1))
        XCTAssertTrue(ConnectionRecovery.shouldShowSlowBanner(failStreak: t))
        XCTAssertTrue(ConnectionRecovery.shouldShowSlowBanner(failStreak: t + 5))
    }

    func testForceReconnectFiresAtTheCrossingButNotOnEveryPoll() {
        let t = ConnectionRecovery.forceReconnectThreshold
        // Below the threshold: don't touch the socket on a transient blip.
        for s in 0..<t {
            XCTAssertFalse(ConnectionRecovery.shouldForceReconnect(failStreak: s),
                           "streak \(s) must not force a drop")
        }
        // At the crossing: drop.
        XCTAssertTrue(ConnectionRecovery.shouldForceReconnect(failStreak: t))
        // Immediately after: do NOT re-drop every poll (that would cancel the
        // in-flight reconnect before its handshake completes).
        for s in (t + 1)..<(2 * t) {
            XCTAssertFalse(ConnectionRecovery.shouldForceReconnect(failStreak: s),
                           "streak \(s) must not re-drop the reconnecting socket")
        }
    }

    /// REGRESSION (stuck-forever): the trigger must RE-ARM on a long outage.
    ///
    /// `reconnectWithBackoff` gives up after ~4 minutes. With a fire-exactly-once
    /// trigger, any outage longer than that budget left the app permanently
    /// disconnected — the streak climbed past the threshold and nothing ever
    /// retried, so the app sat on "Reconnecting to the background helper…" until
    /// it was relaunched by hand (observed live for ~1.5 h after a daemon update).
    func testForceReconnectReArmsSoLongOutagesStillRecover() {
        let t = ConnectionRecovery.forceReconnectThreshold
        // Every further multiple of the threshold re-triggers a reconnect …
        for k in 2...20 {
            XCTAssertTrue(ConnectionRecovery.shouldForceReconnect(failStreak: k * t),
                          "streak \(k * t) must re-arm the reconnect")
        }
        // … so an outage of ANY length keeps producing retries. Over 200 failed
        // polls (~17 min) there must be many attempts, not the single one the
        // old `== threshold` check allowed.
        let attempts = (1...200).filter { ConnectionRecovery.shouldForceReconnect(failStreak: $0) }.count
        XCTAssertGreaterThan(attempts, 10, "recovery must be unbounded, got \(attempts) attempts")
    }

    func testThresholdIsSaneHeartbeatWindow() {
        // 3 consecutive 5s polls ≈ 15s of a dead heartbeat — long enough to be a
        // real drop, short enough to recover quickly.
        XCTAssertGreaterThanOrEqual(ConnectionRecovery.forceReconnectThreshold, 2)
        XCTAssertLessThanOrEqual(ConnectionRecovery.forceReconnectThreshold, 5)
    }
}
