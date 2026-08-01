import XCTest

/// Pure-logic tests for the v1.2 feature batch: clipboard expiry, tunnel port
/// editing, and the post-update credential warm-up offer.
final class FeatureCoresTests: XCTestCase {

    func testTroubleshootRestartPreservesLiveSSHConnections() {
        let args = BackgroundServicePolicy.restartLaunchctlArguments(
            domain: "gui/501", label: "com.ssh2fa.daemon")
        XCTAssertEqual(args, ["kickstart", "gui/501/com.ssh2fa.daemon"])
        XCTAssertFalse(args.contains("-k"),
                       "kickstart -k sends SIGTERM and forces every host through 2FA again")
    }

    func testUIOnlyUpdateDoesNotRestartTheBackgroundService() {
        let old = BackgroundServicePolicy.bundleStamp(
            daemonPath: "/Applications/SSH2FA.app/Contents/Resources/ssh2fa-daemon",
            daemonIdentity: "same-daemon-hash\n", fallbackAppBuild: "160")
        let uiOnlyUpdate = BackgroundServicePolicy.bundleStamp(
            daemonPath: "/Applications/SSH2FA.app/Contents/Resources/ssh2fa-daemon",
            daemonIdentity: "same-daemon-hash", fallbackAppBuild: "161")
        XCTAssertEqual(old, uiOnlyUpdate)
    }

    func testDaemonOrPathChangeStillRequestsAServiceRestart() {
        let baseline = BackgroundServicePolicy.bundleStamp(
            daemonPath: "/Applications/SSH2FA.app/Contents/Resources/ssh2fa-daemon",
            daemonIdentity: "hash-a", fallbackAppBuild: "160")
        let newCode = BackgroundServicePolicy.bundleStamp(
            daemonPath: "/Applications/SSH2FA.app/Contents/Resources/ssh2fa-daemon",
            daemonIdentity: "hash-b", fallbackAppBuild: "160")
        let moved = BackgroundServicePolicy.bundleStamp(
            daemonPath: "/Users/me/SSH2FA.app/Contents/Resources/ssh2fa-daemon",
            daemonIdentity: "hash-a", fallbackAppBuild: "160")
        XCTAssertNotEqual(baseline, newCode)
        XCTAssertNotEqual(baseline, moved)
    }

    func testOldBundleFallsBackToAppBuildForSafety() {
        XCTAssertNotEqual(
            BackgroundServicePolicy.bundleStamp(
                daemonPath: "/daemon", daemonIdentity: nil, fallbackAppBuild: "160"),
            BackgroundServicePolicy.bundleStamp(
                daemonPath: "/daemon", daemonIdentity: "  ", fallbackAppBuild: "161"))
    }

    // MARK: - ClipboardExpiry

    /// The wipe must happen when our copy is still the clipboard's contents.
    func testClearsWhenOurSecretIsStillOnTheClipboard() {
        XCTAssertTrue(ClipboardExpiry.shouldClear(writtenChangeCount: 7, currentChangeCount: 7))
    }

    /// THE important case: if the user copied something else after us, wiping
    /// would destroy their data to protect ours. Never do that.
    func testDoesNotClearWhenSomethingElseWasCopiedAfterwards() {
        XCTAssertFalse(ClipboardExpiry.shouldClear(writtenChangeCount: 7, currentChangeCount: 8))
        XCTAssertFalse(ClipboardExpiry.shouldClear(writtenChangeCount: 7, currentChangeCount: 99))
    }

    /// A secret must not linger indefinitely — the lifetime has to be a real,
    /// short window (long enough to paste, short enough to matter).
    func testSecretLifetimeIsShortButUsable() {
        XCTAssertGreaterThanOrEqual(SecretClipboard.lifetime, 15)
        XCTAssertLessThanOrEqual(SecretClipboard.lifetime, 120)
    }

    // MARK: - TunnelPortEdit

    func testValidPortsPassValidation() {
        XCTAssertNil(TunnelPortEdit.validate(local: "8888", remote: "8888"))
        XCTAssertNil(TunnelPortEdit.validate(local: "1", remote: "65535"))
    }

    /// Out of range / non-numeric / empty must be REFUSED, never clamped — a
    /// silently-changed port sends traffic somewhere the user didn't ask for.
    func testInvalidPortsAreRefusedNotClamped() {
        XCTAssertNotNil(TunnelPortEdit.validate(local: "0", remote: "8888"))
        XCTAssertNotNil(TunnelPortEdit.validate(local: "65536", remote: "8888"))
        XCTAssertNotNil(TunnelPortEdit.validate(local: "8888", remote: "-1"))
        XCTAssertNotNil(TunnelPortEdit.validate(local: "80a", remote: "8888"))
        XCTAssertNotNil(TunnelPortEdit.validate(local: "", remote: "8888"))
        XCTAssertNil(TunnelPortEdit.parse("0"))
        XCTAssertNil(TunnelPortEdit.parse("70000"))
        XCTAssertEqual(TunnelPortEdit.parse(" 8080 "), 8080)
    }

    /// Only changed ports are sent, so the daemon's "nothing to change" guard
    /// stays meaningful.
    func testOnlyChangedPortsAreSent() {
        let none = TunnelPortEdit.changes(local: "8888", remote: "9999",
                                          currentLocal: 8888, currentRemote: 9999)
        XCTAssertNil(none.local)
        XCTAssertNil(none.remote)

        let localOnly = TunnelPortEdit.changes(local: "9001", remote: "9999",
                                               currentLocal: 8888, currentRemote: 9999)
        XCTAssertEqual(localOnly.local, 9001)
        XCTAssertNil(localOnly.remote, "an unchanged remote port must not be sent")

        let both = TunnelPortEdit.changes(local: "1", remote: "2",
                                          currentLocal: 8888, currentRemote: 9999)
        XCTAssertEqual(both.local, 1)
        XCTAssertEqual(both.remote, 2)
    }

    // MARK: - CredentialWarmup

    /// Offered once per build until the one-time stable-vault migration succeeds.
    func testWarmupOfferedOncePerBuild() {
        XCTAssertTrue(CredentialWarmup.shouldOffer(hostCount: 3, currentBuild: "120",
                                                   lastWarmedBuild: "110"),
                      "a new build must re-offer")
        XCTAssertFalse(CredentialWarmup.shouldOffer(hostCount: 3, currentBuild: "120",
                                                    lastWarmedBuild: "120"),
                       "the same build must never nag twice")
    }

    /// A fresh install has nothing recorded — it still needs the pass.
    func testWarmupOfferedWhenNothingRecordedYet() {
        XCTAssertTrue(CredentialWarmup.shouldOffer(hostCount: 1, currentBuild: "120",
                                                   lastWarmedBuild: nil))
    }

    /// A verified stable vault needs neither a banner nor a background probe.
    func testConsolidatedInstallsNeedNoWarmup() {
        XCTAssertFalse(CredentialWarmup.shouldOffer(hostCount: 6, currentBuild: "154",
                                                    lastWarmedBuild: "153", consolidated: true))
    }

    /// An install still holding two items per host DOES need the up-front
    /// explanation — that is a dozen dialogs, not one.
    func testUnconsolidatedInstallsStillGetTheBanner() {
        XCTAssertTrue(CredentialWarmup.shouldOffer(hostCount: 6, currentBuild: "154",
                                                   lastWarmedBuild: "153", consolidated: false))
    }

    /// A dismissed banner may not nag twice in one build.
    func testWarmupDoesNotRunTwiceForTheSameBuild() {
        XCTAssertFalse(CredentialWarmup.shouldOffer(hostCount: 6, currentBuild: "154",
                                                    lastWarmedBuild: "154", consolidated: false))
    }

    func testWarmupNotOfferedWithoutHosts() {
        XCTAssertFalse(CredentialWarmup.shouldOffer(hostCount: 0, currentBuild: "120",
                                                    lastWarmedBuild: nil))
    }

    func testWarmupProgressLabelIsOneBased() {
        XCTAssertEqual(CredentialWarmup.progressLabel(host: "k6", index: 0, total: 6),
                       "Authorizing k6 (1 of 6)…")
        XCTAssertEqual(CredentialWarmup.progressLabel(host: "b8", index: 5, total: 6),
                       "Authorizing b8 (6 of 6)…")
    }

    /// A partial failure must name the hosts that need another try — the fix is
    /// per host, so "some failed" would be useless.
    func testWarmupSummaryNamesFailedHosts() {
        let s = CredentialWarmup.summary(total: 6, failed: ["k8", "b8"])
        XCTAssertTrue(s.contains("k8"))
        XCTAssertTrue(s.contains("b8"))
        XCTAssertTrue(s.contains("4 of 6"))
        XCTAssertTrue(s.contains("final migration pass"))
    }

    /// The summary must say what changed for NEXT time — that is the whole
    /// user-visible payoff of consolidation.
    func testWarmupSummaryMentionsConsolidation() {
        let s = CredentialWarmup.summary(total: 6, failed: [], consolidated: 6)
        XCTAssertTrue(s.contains("stable Keychain vault"))
        XCTAssertTrue(s.contains("future updates"))
    }

    func testWarmupSummaryReportsAnUnverifiedVault() {
        let s = CredentialWarmup.summary(total: 6, failed: [], consolidated: 0,
                                         consolidationSucceeded: false)
        XCTAssertTrue(s.contains("could not be verified"))
    }

    func testWarmupSummarySuccessAndEmptyCases() {
        XCTAssertTrue(CredentialWarmup.summary(total: 3, failed: []).contains("All 3 hosts"))
        XCTAssertTrue(CredentialWarmup.summary(total: 1, failed: []).contains("All 1 host"))
        XCTAssertEqual(CredentialWarmup.summary(total: 0, failed: []),
                       "No saved credentials to authorize.")
    }
}
