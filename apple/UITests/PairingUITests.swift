import XCTest

/// Pairs the app with the fake daemons from the link they share (`XCUIApplication.pairLink`).
final class PairingUITests: XCTestCase {
    @MainActor
    func testPairingWithASharedLinkPairsEveryMachine() throws {
        let link = try XCUIApplication.pairLink()
        let app = XCUIApplication()
        app.launch()

        let add = app.buttons["Add Machine"].firstMatch
        XCTAssertTrue(add.waitForExistence(timeout: 30), "the app did not open on an empty Home")
        add.tap()
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
