import XCTest

final class RecentNodesTests: XCTestCase {
    func testAddsToFront() {
        XCTAssertEqual(RecentNodes.updated(["a", "b"], adding: "c", cap: 8), ["c", "a", "b"])
    }

    func testDedupMovesExistingToFront() {
        XCTAssertEqual(RecentNodes.updated(["a", "b", "c"], adding: "b", cap: 8), ["b", "a", "c"])
    }

    func testCapTrimsOldest() {
        XCTAssertEqual(RecentNodes.updated(["a", "b", "c"], adding: "d", cap: 3), ["d", "a", "b"])
    }

    func testBlankNodeLeavesListUnchanged() {
        XCTAssertEqual(RecentNodes.updated(["a"], adding: "   ", cap: 8), ["a"])
    }

    func testTrimsWhitespace() {
        XCTAssertEqual(RecentNodes.updated([], adding: "  node01 ", cap: 8), ["node01"])
    }
}
