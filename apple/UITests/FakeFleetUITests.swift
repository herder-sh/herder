import XCTest

/// Launches the app on the fake daemons (`XCUIApplication.launchOnFakeFleet`), which it joins
/// on its own, and checks both connect.
final class FakeFleetUITests: XCTestCase {
    @MainActor
    func testTheAppJoinsEveryFakeMachine() throws {
        let app = XCUIApplication()
        try app.launchOnFakeFleet()

        XCTAssertTrue(app.staticTexts["All 2 machines connected"].waitForExistence(timeout: 15))

        // The Board, which no longer leads to Pull Requests.
        XCTAssertTrue(app.navigationBars["Board"].waitForExistence(timeout: 5))
        let prs = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Pull Requests'")).firstMatch
        XCTAssertFalse(prs.exists, "the Board still leads to Pull Requests")
    }
}
