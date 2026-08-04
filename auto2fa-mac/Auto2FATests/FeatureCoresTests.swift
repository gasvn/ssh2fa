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

    func testLaunchctlPIDParserIgnoresUnrelatedNumericFields() {
        let output = """
        gui/501/com.ssh2fa.daemon = {
            runs = 14
            pid = 16283
            forks = 17
        }
        """
        XCTAssertEqual(BackgroundServicePolicy.servicePID(fromLaunchctlPrint: output), 16283)
        XCTAssertNil(BackgroundServicePolicy.servicePID(fromLaunchctlPrint: "state = waiting"))
    }

    func testRunningDaemonMovedIntoBackupForcesRestartEvenWhenCodeIsUnchanged() {
        let expected = "/Applications/SSH2FA.app/Contents/Resources/ssh2fa-daemon"
        XCTAssertFalse(BackgroundServicePolicy.runtimePathNeedsRestart(
            expectedPath: expected, actualPath: expected))
        XCTAssertTrue(BackgroundServicePolicy.runtimePathNeedsRestart(
            expectedPath: expected,
            actualPath: "/private/tmp/SSH2FA.app.old/Contents/Resources/ssh2fa-daemon"))
        XCTAssertFalse(BackgroundServicePolicy.runtimePathNeedsRestart(
            expectedPath: expected, actualPath: nil),
            "an unavailable diagnostic must not churn a healthy service every launch")
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

    /// A verified stable vault needs neither a banner nor a background probe.
    func testConsolidatedInstallsNeedNoWarmup() {
        XCTAssertFalse(CredentialWarmup.shouldOffer(hostCount: 6, consolidated: true,
                                                    deferredForSession: false))
    }

    /// An install still holding two items per host DOES need the up-front
    /// explanation — that is a dozen dialogs, not one.
    func testUnconsolidatedInstallsStillGetTheBanner() {
        XCTAssertTrue(CredentialWarmup.shouldOffer(hostCount: 6, consolidated: false,
                                                   deferredForSession: false))
    }

    /// "Not now" is quiet for this session without falsely recording success.
    func testWarmupDeferralIsSessionScoped() {
        XCTAssertFalse(CredentialWarmup.shouldOffer(hostCount: 6, consolidated: false,
                                                    deferredForSession: true))
        XCTAssertTrue(CredentialWarmup.shouldOffer(hostCount: 6, consolidated: false,
                                                   deferredForSession: false))
    }

    func testWarmupNotOfferedWithoutHosts() {
        XCTAssertFalse(CredentialWarmup.shouldOffer(hostCount: 0, consolidated: false,
                                                    deferredForSession: false))
    }

    func testWarmupSuccessExplainsTheUserBenefitWithoutInternals() {
        let message = CredentialWarmup.successMessage(hostCount: 6)
        XCTAssertTrue(message.contains("should not ask for your Mac password again"))
        XCTAssertFalse(message.localizedCaseInsensitiveContains("keychain"))
        XCTAssertFalse(message.localizedCaseInsensitiveContains("daemon"))
        XCTAssertFalse(message.localizedCaseInsensitiveContains("vault"))
    }

    func testWarmupFailureReassuresThatNothingWasLost() {
        let message = CredentialWarmup.failureMessage()
        XCTAssertTrue(message.localizedCaseInsensitiveContains("migrating"))
        XCTAssertTrue(message.contains("Nothing was lost"))
        XCTAssertFalse(message.localizedCaseInsensitiveContains("keychain"))
    }

    func testMigrationAuthorizationExplainsTheMacPasswordPromptBeforeItAppears() {
        let message = CredentialWarmup.migrationAuthorizationExplanation()
        XCTAssertTrue(message.localizedCaseInsensitiveContains("migrate"))
        XCTAssertTrue(message.contains("not an SSH login"))
        XCTAssertTrue(message.contains("goes only to macOS"))
        XCTAssertTrue(message.contains("SSH2FA never receives it"))
        XCTAssertFalse(message.localizedCaseInsensitiveContains("keychain"))
        XCTAssertFalse(message.localizedCaseInsensitiveContains("daemon"))
    }

    func testWarmupReadyMarkerIsWrittenOnlyAfterVerifiedCompletion() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let marker = directory.appendingPathComponent(CredentialWarmup.readyMarkerName)

        XCTAssertFalse(CredentialWarmup.writeReadyMarkerIfNeeded(
            consolidated: false, markerURL: marker))
        XCTAssertFalse(FileManager.default.fileExists(atPath: marker.path))

        XCTAssertTrue(CredentialWarmup.writeReadyMarkerIfNeeded(
            consolidated: true, markerURL: marker))
        XCTAssertEqual(try String(contentsOf: marker, encoding: .utf8), "ready\n")
        let attrs = try FileManager.default.attributesOfItem(atPath: marker.path)
        XCTAssertEqual(attrs[.posixPermissions] as? NSNumber, NSNumber(value: 0o600))
    }
}
