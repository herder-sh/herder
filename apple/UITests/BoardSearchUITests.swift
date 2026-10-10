import XCTest

/// Searches the Board on iPhone, against the fake daemons of `PairingUITests`: `fake-host-1` has a
/// session waiting on an approval.
final class BoardSearchUITests: XCTestCase {
    @MainActor
    func testSearchingTheBoardFiltersItsSessions() throws {
        let app = XCUIApplication()
        app.launch()
        try app.pairUnlessPaired()
        app.tabBars.buttons["Board"].tap()

        // In sight without pulling the Board down.
        let field = app.searchFields.matching(NSPredicate(format: "placeholderValue == %@", "Sessions")).firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 15))
        field.tap()
        field.typeText("no such session")
        XCTAssertTrue(app.staticTexts["No sessions match."].waitForExistence(timeout: 5))
        let session = app.buttons.containing(NSPredicate(format: "label CONTAINS 'fake-host-1'"))
        XCTAssertEqual(session.count, 0)

        field.buttons["Clear text"].tap()
        field.typeText("fake-host-1")
        XCTAssertTrue(session.firstMatch.waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["No sessions match."].exists)
        // Requests make way for what the search finds.
        XCTAssertFalse(app.descendants(matching: .any)["request-session"].exists)
    }
}
