import XCTest

final class OTPSecretTests: XCTestCase {
    func testFullOtpauthURLPassesThrough() {
        let url = "otpauth://totp/Duo:me?secret=JBSWY3DPEHPK3PXP&issuer=Duo"
        XCTAssertEqual(OTPSecret.normalize(input: url, account: "h"), url)
    }

    func testSecretParamStringPassesThrough() {
        XCTAssertEqual(OTPSecret.normalize(input: "secret=JBSWY3DPEHPK3PXP", account: "h"),
                       "secret=JBSWY3DPEHPK3PXP")
    }

    func testBareBase32KeyIsWrapped() {
        XCTAssertEqual(OTPSecret.normalize(input: "JBSWY3DPEHPK3PXP", account: "kempner"),
                       "otpauth://totp/kempner?secret=JBSWY3DPEHPK3PXP")
    }

    func testBareKeyStripsSpacesAndUppercases() {
        XCTAssertEqual(OTPSecret.normalize(input: "jbsw y3dp ehpk 3pxp", account: "k"),
                       "otpauth://totp/k?secret=JBSWY3DPEHPK3PXP")
    }

    func testBareKeyWithEmptyAccountUsesFallbackLabel() {
        XCTAssertEqual(OTPSecret.normalize(input: "JBSWY3DP", account: "  "),
                       "otpauth://totp/ssh2fa?secret=JBSWY3DP")
    }

    func testEmptyInputIsNil() {
        XCTAssertNil(OTPSecret.normalize(input: "   ", account: "h"))
    }

    func testNonBase32GarbageIsNil() {
        // 0, 1, 8, 9 and punctuation aren't in the base32 alphabet.
        XCTAssertNil(OTPSecret.normalize(input: "not-a-secret!", account: "h"))
        XCTAssertNil(OTPSecret.normalize(input: "10189", account: "h"))
    }
}
