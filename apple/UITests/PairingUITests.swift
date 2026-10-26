import XCTest

/// Pairs the app with the fake daemons from the link they share (`XCUIApplication.pairLink`).
final class PairingUITests: XCTestCase {
    @MainActor
    func testPairingWithASharedLinkPairsEveryMachine() throws {
        let link = try XCUIApplication.pairLink()
        let app = XCUIApplication()
        app.launch()

        let add = app.buttons["Add Machine"].firstMatch
        XCTAssertTrue(add.waitForExistence(timeout: 30), "the app did not open on an empty Board")
        let field = app.openPairing(add)
        field.tap()
        field.typeText(link)
        XCTAssertTrue(app.staticTexts["Check these 2 machines, then pair"].waitForExistence(timeout: 5))
        app.buttons["Pair"].tap()

        // Each machine's result.
        XCTAssertTrue(app.staticTexts["fake-host-1"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.staticTexts["fake-host-2"].exists)
        XCTAssertEqual(app.staticTexts.matching(identifier: "Paired").count, 2)
        app.buttons["Done"].tap()

        XCTAssertTrue(app.staticTexts["All 2 machines connected"].waitForExistence(timeout: 15))

        // The Board, which no longer leads to Pull Requests.
        app.tabBars.buttons["Board"].tap()
        XCTAssertTrue(app.navigationBars["Board"].waitForExistence(timeout: 5))
        let prs = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Pull Requests'")).firstMatch
        XCTAssertFalse(prs.exists, "the Board still leads to Pull Requests")
    }
}
