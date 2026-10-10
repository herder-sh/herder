import XCTest

/// Searches the Board on iPhone, against the fake daemons of `PairingUITests`: `fake-host-1` has a
/// session that was asked to "Run the tests.".
final class BoardSearchUITests: XCTestCase {
    @MainActor
    func testSearchingTheBoardFiltersItsSessions() throws {
        let app = XCUIApplication()
        app.launch()
        try app.pairUnlessPaired()
        app.tabBars.buttons["Board"].tap()

        let field = app.searchFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 15), app.debugDescription)
        field.tap()
        field.typeText("no such session")
        XCTAssertTrue(app.staticTexts["No sessions match."].waitForExistence(timeout: 5))

        field.buttons["Clear text"].tap()
        field.typeText("tests")
        let session = app.staticTexts.containing(NSPredicate(format: "label CONTAINS 'Run the tests'")).firstMatch
        XCTAssertTrue(session.waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["No sessions match."].exists)
    }
}
