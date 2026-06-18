import XCTest

final class OnboardingChecklistTests: XCTestCase {
    func testNothingDoneAtStart() {
        XCTAssertEqual(OnboardingChecklist.completed(hostCount: 0, anyConnected: false, usedTerminal: false), [])
    }

    func testStepsCompleteIndependently() {
        XCTAssertEqual(OnboardingChecklist.completed(hostCount: 1, anyConnected: false, usedTerminal: false), [.addHost])
        XCTAssertEqual(OnboardingChecklist.completed(hostCount: 2, anyConnected: true, usedTerminal: false), [.addHost, .seeConnect])
        XCTAssertEqual(OnboardingChecklist.completed(hostCount: 1, anyConnected: true, usedTerminal: true),
                       [.addHost, .seeConnect, .openTerminal])
    }

    func testShowsWhileIncomplete() {
        XCTAssertTrue(OnboardingChecklist.shouldShow(hostCount: 0, anyConnected: false, usedTerminal: false))
        XCTAssertTrue(OnboardingChecklist.shouldShow(hostCount: 1, anyConnected: false, usedTerminal: false))
        XCTAssertTrue(OnboardingChecklist.shouldShow(hostCount: 1, anyConnected: true, usedTerminal: false))
    }

    func testHiddenOnceAllDone() {
        XCTAssertFalse(OnboardingChecklist.shouldShow(hostCount: 1, anyConnected: true, usedTerminal: true))
    }
}
