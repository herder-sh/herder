import XCTest

/// A fresh install with no daemon reaches a populated Board through the demo, and leaves it
/// for the real empty state: what App Review does.
final class DemoUITests: XCTestCase {
    @MainActor
    func testFreshInstallExploresTheDemo() throws {
        let app = XCUIApplication()
        app.launchEnvironment["HERDER_PROFILE"] = "demo-\(UUID().uuidString)"
        app.launch()

        let explore = app.buttons["explore-demo"].firstMatch
        XCTAssertTrue(explore.waitForExistence(timeout: 15), "the empty state offers no demo")
        explore.tap()
        app.tabBars.buttons["Board"].tap()

        XCTAssertTrue(app.buttons["leave-demo"].waitForExistence(timeout: 10), "the demo is not marked")
        let waiting = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Waiting for you, '")).firstMatch
        XCTAssertTrue(waiting.waitForExistence(timeout: 10), "the demo Board has no task waiting on you")
        XCTAssertFalse(app.buttons["add-to-fleet"].exists, "the demo Board is empty")

        app.buttons["leave-demo"].tap()
        XCTAssertTrue(app.buttons["explore-demo"].firstMatch.waitForExistence(timeout: 10),
                      "leaving the demo did not return to the empty state")
        XCTAssertFalse(app.buttons["leave-demo"].exists)
    }
}
