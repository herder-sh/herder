import XCTest

/// Searches the Board on iPhone, against the fake daemons of `PairingUITests`: `fake-host-1` has a
/// session waiting on an approval.
final class SessionSearchUITests: XCTestCase {
    @MainActor
    func testSearchingTheBoardFiltersItsSessions() throws {
        let app = XCUIApplication()
        app.launch()
        try app.pairUnlessPaired()
        app.tabBars.buttons["Board"].tap()

        // In sight without pulling the Board down.
        let field = app.searchFields.matching(NSPredicate(format: "placeholderValue == %@", "Tasks")).firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 15))
        field.tap()
        field.typeText("no such task")
        XCTAssertTrue(app.staticTexts["No tasks match."].waitForExistence(timeout: 5))
        let task = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Waiting for you, '"))
        XCTAssertEqual(task.count, 0)

        // A task is found by its machine's name too.
        field.buttons["Clear text"].tap()
        field.typeText("fake-host-1")
        XCTAssertTrue(task.firstMatch.waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["No tasks match."].exists)
    }
}
