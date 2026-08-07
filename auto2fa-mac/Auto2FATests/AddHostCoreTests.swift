import XCTest

/// The Add-host wizard's 2FA rules. The point of these: an account WITHOUT 2FA
/// must be addable (blank field), while a typo must still be caught at entry —
/// the two must never be confused, since confusing them silently downgrades a
/// 2FA host to password-only and fails at the next login prompt.
final class AddHostCoreTests: XCTestCase {

    // MARK: - classifyOTP

    func testBlankFieldMeansNo2FA() {
        XCTAssertEqual(AddHostCore.classifyOTP(input: "", account: "k6"), .none)
        XCTAssertEqual(AddHostCore.classifyOTP(input: "   ", account: "k6"), .none)
        XCTAssertEqual(AddHostCore.classifyOTP(input: "\n\t ", account: "k6"), .none)
    }

    func testOtpauthURLPassesThrough() {
        let url = "otpauth://totp/Example:alice?secret=JBSWY3DPEHPK3PXP&issuer=Example"
        XCTAssertEqual(AddHostCore.classifyOTP(input: url, account: "k6"), .secret(url))
    }

    func testBareBase32KeyIsWrappedIntoAURL() {
        guard case .secret(let url) = AddHostCore.classifyOTP(input: "jbswy3dpehpk3pxp",
                                                              account: "k6") else {
            return XCTFail("a bare base32 key is a valid secret")
        }
        XCTAssertEqual(url, "otpauth://totp/k6?secret=JBSWY3DPEHPK3PXP")
    }

    func testTypedGarbageIsInvalidNotAbsent() {
        // The distinction that matters: these must NOT read as "no 2FA".
        // Both are what people actually paste by mistake — the 6-digit code the
        // authenticator is showing, and the enrolment page's link.
        XCTAssertEqual(AddHostCore.classifyOTP(input: "483920", account: "k6"), .invalid)
        XCTAssertEqual(AddHostCore.classifyOTP(input: "https://duo.example.edu/enroll",
                                               account: "k6"), .invalid)
        XCTAssertEqual(AddHostCore.classifyOTP(input: "1234-5678", account: "k6"), .invalid)
    }

    // MARK: - otpauthPayload

    func testPayloadIsEmptyForAPasswordOnlyHost() {
        XCTAssertEqual(AddHostCore.otpauthPayload(.none), "")
        // Never smuggle unparseable text into the Keychain.
        XCTAssertEqual(AddHostCore.otpauthPayload(.invalid), "")
        XCTAssertEqual(AddHostCore.otpauthPayload(.secret("otpauth://x")), "otpauth://x")
    }

    // MARK: - credentialsError

    func testBlank2FAIsAcceptedSoAPasswordOnlyAccountCanBeAdded() {
        XCTAssertNil(AddHostCore.credentialsError(password: "pw", otpInput: "", alias: "k6"))
        XCTAssertNil(AddHostCore.credentialsError(password: "pw", otpInput: "  ", alias: "k6"))
    }

    func testValidSecretIsAccepted() {
        XCTAssertNil(AddHostCore.credentialsError(password: "pw",
                                                  otpInput: "JBSWY3DPEHPK3PXP",
                                                  alias: "k6"))
    }

    func testPasswordIsStillRequired() {
        let msg = AddHostCore.credentialsError(password: "", otpInput: "", alias: "k6")
        XCTAssertEqual(msg, "Password is required.")
    }

    func testUnusableSecretIsStillRejected() {
        // The current 6-digit code, pasted where the setup key belongs.
        let msg = AddHostCore.credentialsError(password: "pw", otpInput: "483920", alias: "k6")
        XCTAssertNotNil(msg, "a typo must not be accepted as a password-only host")
        // The message has to name the way out, or a user with no 2FA is stuck.
        XCTAssertTrue(msg?.contains("leave it empty") == true, "got \(msg ?? "nil")")
    }

    // MARK: - otpSummary

    func testSummaryReadsAsAChoiceNotAFailure() {
        // A password-only host is a normal outcome; its summary must not look
        // like the validation error that `.invalid` produces.
        let none = AddHostCore.otpSummary(.none)
        XCTAssertTrue(none.contains("password only"), "got \(none)")
        XCTAssertNotEqual(none, AddHostCore.otpSummary(.invalid))
        XCTAssertEqual(AddHostCore.otpSummary(.secret("x")), "ready")
    }
}
