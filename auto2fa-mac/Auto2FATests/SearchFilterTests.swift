import XCTest

final class SearchFilterTests: XCTestCase {
    func testBlankQueryMatchesEverything() {
        XCTAssertTrue(SearchFilter.matches(query: "", in: ["kempner"]))
        XCTAssertTrue(SearchFilter.matches(query: "   ", in: [nil]))
    }

    func testCaseInsensitiveSubstring() {
        XCTAssertTrue(SearchFilter.matches(query: "KEMP", in: ["kempner-login"]))
        XCTAssertTrue(SearchFilter.matches(query: "node01", in: [nil, "Node01", nil]))
    }

    func testNoMatchReturnsFalse() {
        XCTAssertFalse(SearchFilter.matches(query: "zzz", in: ["kempner", "txgent", nil]))
    }

    func testNilFieldsAreSkipped() {
        XCTAssertFalse(SearchFilter.matches(query: "x", in: [nil, nil]))
    }
}
