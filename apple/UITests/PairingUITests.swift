import XCTest

/// Drives the app against the fake daemon whose pairing link is in `HERDER_PAIR_LINK` (pass it
/// to `xcodebuild test` as `TEST_RUNNER_HERDER_PAIR_LINK`).
final class PairingUITests: XCTestCase {
    func testPairingWithALinkShowsTheMachineConnected() throws {
        let link = try XCTUnwrap(
            ProcessInfo.processInfo.environment["HERDER_PAIR_LINK"], "needs HERDER_PAIR_LINK")
        let app = XCUIApplication()
        app.launch()

        app.buttons["Add Machine"].firstMatch.tap()
        let field = app.descendants(matching: .any)["pairing-link"].firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.tap()
        field.typeText(link)
        app.buttons["Pair"].tap()

        XCTAssertTrue(app.staticTexts["1 machine connected"].waitForExistence(timeout: 15))
        app.tabBars.buttons["Machines"].tap()
        XCTAssertTrue(app.staticTexts["Connected"].waitForExistence(timeout: 5))
    }
}
