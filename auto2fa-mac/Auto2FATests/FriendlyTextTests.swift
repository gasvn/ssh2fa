import XCTest

/// Regression tests for the direct-mode status copy: an idle DIRECT tunnel must
/// never be told to "Pick a compute node to start" (it has no node), while the
/// compute path keeps its original behavior.
final class FriendlyTextTests: XCTestCase {

    /// Decode a Tunnel from a daemon-style snapshot. `directHost`/`lastNode`
    /// default to nil; `status`/`lastMsg` are caller-controlled.
    private func makeTunnel(status: String,
                            directHost: String? = nil,
                            lastNode: String? = nil,
                            lastMsg: String = "") throws -> Tunnel {
        func q(_ s: String?) -> String { s.map { "\"\($0)\"" } ?? "null" }
        let json = """
        {
          "name": "web", "local_port": 9000, "remote_port": 9000,
          "jump_candidates": null, "last_node": \(q(lastNode)),
          "last_user": null, "direct_host": \(q(directHost)),
          "auto_start": false, "post_connect_cmd": null, "tags": [],
          "url_path": null, "active_jump": null, "status": "\(status)",
          "last_msg": "\(lastMsg)", "last_alive_at": 0.0,
          "total_uptime_sec": 0.0, "connect_count": 0, "fail_count": 0
        }
        """
        return try JSONDecoder().decode(Tunnel.self, from: Data(json.utf8))
    }

    func testDirectFlagDecodes() throws {
        let direct = try makeTunnel(status: "idle", directHost: "loginhost")
        XCTAssertTrue(direct.isDirect)
        XCTAssertEqual(direct.directHost, "loginhost")
        let compute = try makeTunnel(status: "idle")
        XCTAssertFalse(compute.isDirect)
    }

    /// The bug: an idle direct tunnel showed "Pick a compute node to start".
    /// It must instead surface the daemon message, or plain "Idle".
    func testIdleDirectTunnelDoesNotSayPickNode() throws {
        let parked = try makeTunnel(status: "idle", directHost: "loginhost",
                                    lastMsg: "waiting for host loginhost")
        let blurb = FriendlyText.tunnelStatusBlurb(parked)
        XCTAssertEqual(blurb, "waiting for host loginhost")
        XCTAssertFalse(blurb.contains("compute node"))
    }

    func testIdleDirectTunnelEmptyMessageIsIdle() throws {
        let t = try makeTunnel(status: "idle", directHost: "loginhost", lastMsg: "")
        XCTAssertEqual(FriendlyText.tunnelStatusBlurb(t), "Idle")
    }

    /// Compute path unchanged: an idle compute tunnel with no node still gets
    /// the "pick a node" nudge.
    func testIdleComputeTunnelNoNodeStillSaysPickNode() throws {
        let t = try makeTunnel(status: "idle", directHost: nil, lastNode: nil)
        XCTAssertEqual(FriendlyText.tunnelStatusBlurb(t), "Pick a compute node to start")
    }

    func testIdleComputeTunnelWithNodeIsIdle() throws {
        let t = try makeTunnel(status: "idle", directHost: nil, lastNode: "gpunode01")
        XCTAssertEqual(FriendlyText.tunnelStatusBlurb(t), "Idle")
    }

    // MARK: - credentialError (per-host Password & setup failures)

    /// The exact raw string the daemon returns when macOS denies the Keychain
    /// prompt. Observed live after a helper update: `keyring get(k8.password):
    /// Platform secure storage failure: User canceled the operation.`
    func testKeychainDenialExplainsTheOneTimeMigration() {
        let msg = FriendlyText.credentialError(
            "internal: keyring get(k8.password): Platform secure storage failure: User canceled the operation.")
        XCTAssertTrue(msg.contains("Allow"), "must name the button that fixes it: \(msg)")
        XCTAssertTrue(msg.contains("one-time migration"))
        XCTAssertFalse(msg.contains("keyring"), "must not leak the raw API name: \(msg)")
    }

    func testKeychainTimeoutPointsAtThePendingPrompt() {
        let msg = FriendlyText.credentialError(
            "internal: credential read timed out for b8 (is the login Keychain locked?) — try again")
        XCTAssertTrue(msg.contains("Allow"))
        XCTAssertFalse(msg.contains("credential read timed out"))
    }

    func testBusyLatchIsAPlainRetryMessage() {
        let msg = FriendlyText.credentialError(
            "internal: credential read already in flight for k6 — try again")
        XCTAssertTrue(msg.lowercased().contains("try again"))
        XCTAssertFalse(msg.contains("in flight"), "internal term, not user-facing: \(msg)")
    }

    /// An app newer than the running helper must say what to DO, not report
    /// "unknown method".
    func testVersionSkewSaysToReopenTheApp() {
        let msg = FriendlyText.credentialError("unknown method host_credentials")
        XCTAssertTrue(msg.contains("reopen") || msg.contains("Quit"))
        XCTAssertFalse(msg.contains("unknown method"))
        XCTAssertFalse(msg.lowercased().contains("helper"))
        XCTAssertFalse(msg.lowercased().contains("daemon"))
    }

    func testDisconnectedMessageDoesNotExposeBackgroundInternals() {
        let msg = FriendlyText.friendlyError("not connected to ssh2fa-daemon")
        XCTAssertTrue(msg.contains("SSH2FA"))
        XCTAssertTrue(msg.contains("reopen"))
        XCTAssertFalse(msg.lowercased().contains("helper"))
        XCTAssertFalse(msg.lowercased().contains("daemon"))
    }

    func testBackgroundTimeoutIsNotMisreportedAsARemoteServerFailure() {
        let msg = FriendlyText.friendlyError(
            "daemon timed out after 10s on list_hosts")
        XCTAssertTrue(msg.contains("SSH2FA"))
        XCTAssertTrue(msg.lowercased().contains("try again"))
        XCTAssertFalse(msg.lowercased().contains("server"))
        XCTAssertFalse(msg.lowercased().contains("daemon"))
        XCTAssertFalse(msg.contains("list_hosts"))
    }

    func testLockedKeychainSaysToUnlockIt() {
        let msg = FriendlyText.credentialError("the keychain is locked")
        XCTAssertTrue(msg.lowercased().contains("unlock"))
    }

    /// Unrecognized text still falls through to friendlyError (and ultimately
    /// passes through unchanged) rather than being replaced with a guess.
    func testUnknownCredentialErrorFallsThroughToFriendlyError() {
        XCTAssertEqual(FriendlyText.credentialError("some novel failure"), "some novel failure")
        // And it inherits friendlyError's translations.
        XCTAssertEqual(FriendlyText.credentialError("Permission denied, please try again"),
                       FriendlyText.friendlyError("Permission denied, please try again"))
    }

    // MARK: - actionable SSH login failures

    func testWrongPasswordNamesTheRepairSurface() {
        let msg = FriendlyText.friendlyError("Password rejected (Permission denied)")
        XCTAssertTrue(msg.contains("Password & setup"))
        XCTAssertTrue(msg.contains("Test login"))
    }

    func testWrongOTPNamesSecretAndClock() {
        let msg = FriendlyText.friendlyError("Verification code rejected")
        XCTAssertTrue(msg.contains("2FA"))
        XCTAssertTrue(msg.lowercased().contains("date") || msg.lowercased().contains("time"))
    }

    func testSessionLimitNamesServerSideFix() {
        let msg = FriendlyText.friendlyError("Server session limit reached")
        XCTAssertTrue(msg.contains("MaxSessions") || msg.contains("PAM"))
        XCTAssertTrue(msg.lowercased().contains("administrator"))
    }

    func testDnsFailureNamesHostAndVPN() {
        let msg = FriendlyText.friendlyError("Could not resolve hostname")
        XCTAssertTrue(msg.contains("HostName"))
        XCTAssertTrue(msg.contains("VPN"))
    }

    func testDaemonActionableMessageIsMappedThroughHostLastMsg() {
        let msg = FriendlyText.hostLastMsg(
            "The server rejected the password. Open Password & setup and replace the saved password, then use Test login.")
        XCTAssertEqual(
            msg,
            String(localized: "The server rejected the password. Open Password & setup, replace it, then run Test login.")
        )
    }

    func testPermissionDeniedNoLongerSuggestsRemovingTheHost() {
        let msg = FriendlyText.friendlyError("Permission denied")
        XCTAssertTrue(msg.contains("Password & setup"))
        XCTAssertTrue(msg.contains("Test login"))
        XCTAssertFalse(msg.lowercased().contains("re-add"))
    }

    func testHostKeyFailureNamesSafeRepair() {
        let msg = FriendlyText.friendlyError("Host key verification failed")
        XCTAssertTrue(msg.lowercased().contains("fingerprint"))
        XCTAssertTrue(msg.contains("known_hosts"))
    }
}
