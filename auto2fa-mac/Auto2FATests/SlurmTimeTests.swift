import XCTest

final class SlurmTimeTests: XCTestCase {
    func testHHMMSS() {
        XCTAssertEqual(SlurmTime.seconds("2:14:03"), 2 * 3600 + 14 * 60 + 3)
    }

    func testMMSS() {
        XCTAssertEqual(SlurmTime.seconds("30:45"), 30 * 60 + 45)
    }

    func testDaysHHMMSS() {
        XCTAssertEqual(SlurmTime.seconds("1-12:00:00"), 86400 + 12 * 3600)
    }

    func testSecondsOnly() {
        XCTAssertEqual(SlurmTime.seconds("45"), 45)
    }

    func testUnlimitedAndInvalidAreNil() {
        XCTAssertNil(SlurmTime.seconds("UNLIMITED"))
        XCTAssertNil(SlurmTime.seconds("INVALID"))
        XCTAssertNil(SlurmTime.seconds("NOT_SET"))
        XCTAssertNil(SlurmTime.seconds(""))
        XCTAssertNil(SlurmTime.seconds("garbage"))
    }

    func testFormat() {
        XCTAssertEqual(SlurmTime.format(remaining: 2 * 3600 + 14 * 60 + 3), "2:14:03")
        XCTAssertEqual(SlurmTime.format(remaining: 4 * 60 + 9), "4:09")
        XCTAssertEqual(SlurmTime.format(remaining: 0), "expired")
        XCTAssertEqual(SlurmTime.format(remaining: -5), "expired")
    }
}
