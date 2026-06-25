import XCTest

final class SearchFilterTests: XCTestCase {
    func testBlankQueryMatchesEverything() {
        XCTAssertTrue(SearchFilter.matches(query: "", in: ["login01"]))
        XCTAssertTrue(SearchFilter.matches(query: "   ", in: [nil]))
    }

    func testCaseInsensitiveSubstring() {
        XCTAssertTrue(SearchFilter.matches(query: "LOGIN", in: ["login01-login"]))
        XCTAssertTrue(SearchFilter.matches(query: "node01", in: [nil, "Node01", nil]))
    }

    func testNoMatchReturnsFalse() {
        XCTAssertFalse(SearchFilter.matches(query: "zzz", in: ["login01", "bastion01", nil]))
    }

    func testNilFieldsAreSkipped() {
        XCTAssertFalse(SearchFilter.matches(query: "x", in: [nil, nil]))
    }
}
