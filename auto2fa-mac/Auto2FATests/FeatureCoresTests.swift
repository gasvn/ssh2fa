import XCTest

/// Pure-logic tests for the v1.2 feature batch: clipboard expiry, tunnel port
/// editing, and the post-update credential warm-up offer.
final class FeatureCoresTests: XCTestCase {

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

    /// Offered once per BUILD: each build ships a new helper binary, which macOS
    /// treats as a new reader of every saved Keychain item.
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

    /// Nothing to authorize with no hosts — never show the banner on a fresh,
    /// empty install where it would be pure noise.
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
        XCTAssertTrue(s.contains("Always Allow"), "must name the button that fixes it")
    }

    func testWarmupSummarySuccessAndEmptyCases() {
        XCTAssertTrue(CredentialWarmup.summary(total: 3, failed: []).contains("All 3 hosts"))
        XCTAssertTrue(CredentialWarmup.summary(total: 1, failed: []).contains("All 1 host"))
        XCTAssertEqual(CredentialWarmup.summary(total: 0, failed: []),
                       "No saved credentials to authorize.")
    }
}
