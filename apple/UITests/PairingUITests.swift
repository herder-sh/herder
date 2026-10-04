import XCTest

/// Drives the app against the fake daemons whose shared pairing link is in `HERDER_PAIR_LINK`
/// (`fake_daemon --share` prints it; pass it to `xcodebuild test` as
/// `TEST_RUNNER_HERDER_PAIR_LINK`).
final class PairingUITests: XCTestCase {
    @MainActor
    func testPairingWithASharedLinkPairsEveryMachine() throws {
        let link = try XCTUnwrap(
            ProcessInfo.processInfo.environment["HERDER_PAIR_LINK"], "needs HERDER_PAIR_LINK")
        let app = XCUIApplication()
        app.launch()

        app.buttons["Add Machine"].firstMatch.tap()
        let field = app.descendants(matching: .any)["pairing-link"].firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.tap()
        field.typeText(link)
        XCTAssertTrue(app.staticTexts["3 · CHECK THEY ARE YOUR 2 MACHINES"].waitForExistence(timeout: 5))
        app.buttons["Pair"].tap()

        // Each machine's result.
        XCTAssertTrue(app.staticTexts["fake-host-1"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.staticTexts["fake-host-2"].exists)
        XCTAssertEqual(app.staticTexts.matching(identifier: "Paired").count, 2)
        app.buttons["Done"].tap()

        XCTAssertTrue(app.staticTexts["All 2 machines connected"].waitForExistence(timeout: 15))

        // The tab bar reaches every section the Mac sidebar has.
        app.tabBars.buttons["PRs"].tap()
        XCTAssertTrue(app.staticTexts["No open pull requests."].waitForExistence(timeout: 5))
    }
}
