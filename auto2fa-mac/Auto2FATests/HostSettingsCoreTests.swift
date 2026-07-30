import XCTest

/// Tests for the pure logic behind the per-host "Password & setup" sheet.
final class HostSettingsCoreTests: XCTestCase {

    private func conn(_ alias: String, _ host: String, _ user: String, _ port: Int = 22) -> ManagedHostConn {
        ManagedHostConn(alias: alias, hostName: host, user: user, port: port)
    }

    // MARK: - connectionSource

    func testSidecarEntryIsManagedAndEditable() {
        let src = HostSettingsCore.connectionSource(
            alias: "k6",
            sidecar: [conn("k6", "login.example.edu", "alice")],
            configHosts: [])
        XCTAssertEqual(src, .managed(conn("k6", "login.example.edu", "alice")))
        XCTAssertTrue(HostSettingsCore.isEditable(src))
    }

    func testConfigHostIsUserOwnedAndNotEditable() {
        let src = HostSettingsCore.connectionSource(
            alias: "k6",
            sidecar: [],
            configHosts: [ConfigHost(alias: "k6", hostName: "login.example.edu", user: "alice")])
        XCTAssertEqual(src, .userConfig(hostName: "login.example.edu", user: "alice"))
        XCTAssertFalse(HostSettingsCore.isEditable(src),
                       "we must never rewrite the user's own ~/.ssh/config")
    }

    /// The sidecar wins: for a guided host the managed conf is included AHEAD of
    /// the user's config in the daemon's `ssh -F` wrapper, so its values apply.
    func testSidecarWinsOverConfigForTheSameAlias() {
        let src = HostSettingsCore.connectionSource(
            alias: "k6",
            sidecar: [conn("k6", "managed.example.edu", "bob")],
            configHosts: [ConfigHost(alias: "k6", hostName: "config.example.edu", user: "alice")])
        XCTAssertEqual(src, .managed(conn("k6", "managed.example.edu", "bob")))
    }

    /// ssh `Host` matching is case-insensitive, so the lookup must be too.
    func testConfigLookupIsCaseInsensitive() {
        let src = HostSettingsCore.connectionSource(
            alias: "K6",
            sidecar: [],
            configHosts: [ConfigHost(alias: "k6", hostName: "h", user: "u")])
        XCTAssertEqual(src, .userConfig(hostName: "h", user: "u"))
    }

    func testRegisteredButUnresolvableAliasIsUnknown() {
        let src = HostSettingsCore.connectionSource(alias: "ghost", sidecar: [], configHosts: [])
        XCTAssertEqual(src, .unknown)
        XCTAssertFalse(HostSettingsCore.isEditable(src))
    }

    // MARK: - targetSummary

    func testTargetSummaryForManagedHost() {
        XCTAssertEqual(
            HostSettingsCore.targetSummary(.managed(conn("k6", "login.example.edu", "alice"))),
            "alice@login.example.edu")
    }

    func testTargetSummaryShowsNonDefaultPortOnly() {
        XCTAssertEqual(
            HostSettingsCore.targetSummary(.managed(conn("k6", "h", "u", 2222))),
            "u@h:2222")
        XCTAssertEqual(
            HostSettingsCore.targetSummary(.managed(conn("k6", "h", "u", 22))),
            "u@h", "port 22 is the default — don't clutter the summary with it")
    }

    func testTargetSummaryForConfigHostWithPartialInfo() {
        XCTAssertEqual(
            HostSettingsCore.targetSummary(.userConfig(hostName: "h.example.edu", user: nil)),
            "h.example.edu")
        XCTAssertNil(HostSettingsCore.targetSummary(.userConfig(hostName: nil, user: nil)))
        XCTAssertNil(HostSettingsCore.targetSummary(.unknown))
    }

    // MARK: - connectionError / parsePort

    func testValidConnectionHasNoError() {
        XCTAssertNil(HostSettingsCore.connectionError(
            hostName: "login.example.edu", user: "alice", portText: "22"))
        XCTAssertNil(HostSettingsCore.connectionError(
            hostName: "login.example.edu", user: "alice", portText: ""),
            "an empty port means the ssh default")
    }

    func testMissingFieldsAreRejected() {
        XCTAssertNotNil(HostSettingsCore.connectionError(hostName: "  ", user: "a", portText: "22"))
        XCTAssertNotNil(HostSettingsCore.connectionError(hostName: "h", user: " ", portText: "22"))
    }

    /// A space or newline in a value would split into a second ssh config token
    /// (or inject a whole directive) — refuse instead of silently mangling it.
    func testWhitespaceInsideValuesIsRejected() {
        XCTAssertNotNil(HostSettingsCore.connectionError(
            hostName: "login example.edu", user: "alice", portText: "22"))
        XCTAssertNotNil(HostSettingsCore.connectionError(
            hostName: "h\nProxyCommand nc evil 22", user: "alice", portText: "22"))
        XCTAssertNotNil(HostSettingsCore.connectionError(
            hostName: "h", user: "al ice", portText: "22"))
    }

    func testPortRangeIsEnforced() {
        XCTAssertEqual(HostSettingsCore.parsePort("2222"), 2222)
        XCTAssertEqual(HostSettingsCore.parsePort(""), 22)
        XCTAssertEqual(HostSettingsCore.parsePort("  "), 22)
        XCTAssertNil(HostSettingsCore.parsePort("0"))
        XCTAssertNil(HostSettingsCore.parsePort("65536"))
        XCTAssertNil(HostSettingsCore.parsePort("-1"))
        XCTAssertNil(HostSettingsCore.parsePort("22a"))
        XCTAssertNotNil(HostSettingsCore.connectionError(
            hostName: "h", user: "u", portText: "99999"))
    }

    // MARK: - passwordMask

    func testPasswordMaskReflectsLengthButIsClamped() {
        XCTAssertEqual(HostSettingsCore.passwordMask(length: 4), "••••")
        XCTAssertEqual(HostSettingsCore.passwordMask(length: 16).count, 16)
        XCTAssertEqual(HostSettingsCore.passwordMask(length: 200).count, 16,
                       "a long passphrase must not blow out the row")
        XCTAssertEqual(HostSettingsCore.passwordMask(length: 0).count, 1)
    }

    // MARK: - otpSummary

    func testOTPSummaryShowsIssuerAndAccount() {
        XCTAssertEqual(
            HostSettingsCore.otpSummary(issuer: "Duo", account: "alice",
                                        algorithm: "SHA1", digits: 6, period: 30),
            "Duo: alice")
    }

    func testOTPSummaryOmitsDefaultParameters() {
        let s = HostSettingsCore.otpSummary(issuer: nil, account: "alice",
                                            algorithm: "SHA1", digits: 6, period: 30)
        XCTAssertEqual(s, "alice")
        XCTAssertFalse(s.contains("SHA1"))
        XCTAssertFalse(s.contains("30"))
    }

    func testOTPSummarySurfacesNonDefaultParameters() {
        let s = HostSettingsCore.otpSummary(issuer: "Ex", account: "bob",
                                            algorithm: "sha256", digits: 8, period: 60)
        XCTAssertTrue(s.contains("Ex: bob"))
        XCTAssertTrue(s.contains("SHA256"))
        XCTAssertTrue(s.contains("8 digits"))
        XCTAssertTrue(s.contains("60s"))
    }

    /// A canonical token stored by host_add carries no issuer/account — the
    /// summary must still say something rather than render empty.
    func testOTPSummaryWithoutMetadata() {
        XCTAssertEqual(
            HostSettingsCore.otpSummary(issuer: nil, account: nil,
                                        algorithm: "SHA1", digits: 6, period: 30),
            "Stored (no account details)")
    }

    // MARK: - pendingChanges

    func testUntouchedFieldsSendNothing() {
        let c = HostSettingsCore.pendingChanges(newPassword: "", newOTPInput: "", alias: "k6")
        XCTAssertNil(c.password, "an empty field must keep the stored password")
        XCTAssertNil(c.otpauthURL, "an empty field must keep the stored 2FA secret")
    }

    func testTypedPasswordIsSentVerbatim() {
        // Leading/trailing spaces can be REAL in a password — never trim it.
        let c = HostSettingsCore.pendingChanges(newPassword: " hunter2 ", newOTPInput: "", alias: "k6")
        XCTAssertEqual(c.password, " hunter2 ")
    }

    func testBareSecretIsNormalizedToAnOtpauthURL() {
        let c = HostSettingsCore.pendingChanges(newPassword: "",
                                                newOTPInput: "JBSWY3DPEHPK3PXP",
                                                alias: "k6")
        XCTAssertEqual(c.otpauthURL, "otpauth://totp/k6?secret=JBSWY3DPEHPK3PXP")
    }

    func testFullOtpauthURLPassesThrough() {
        let url = "otpauth://totp/Duo:alice?secret=JBSWY3DPEHPK3PXP&issuer=Duo"
        let c = HostSettingsCore.pendingChanges(newPassword: "", newOTPInput: url, alias: "k6")
        XCTAssertEqual(c.otpauthURL, url)
    }

    /// Unnormalizable input is forwarded as typed so the DAEMON produces the
    /// authoritative "invalid otpauth URL" error, instead of the app silently
    /// dropping the field (which would look like a successful save).
    func testUnparseableSecretIsStillForwarded() {
        let c = HostSettingsCore.pendingChanges(newPassword: "",
                                                newOTPInput: "not a secret!",
                                                alias: "k6")
        XCTAssertEqual(c.otpauthURL, "not a secret!")
    }
}
