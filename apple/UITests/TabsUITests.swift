import XCTest

/// With no chats, the Chats tab says so across the screen, once. Against the fake daemons of
/// `PairingUITests`, which have none, pairing first unless an earlier test did.
final class TabsUITests: XCTestCase {
    @MainActor
    func testNoChatsFillsTheScreen() throws {
        let app = XCUIApplication()
        app.launch()
        try app.pairUnlessPaired()

        app.tabBars.buttons["Chats"].tap()
        let note = app.staticTexts["Ask anything, about no project."]
        XCTAssertTrue(note.waitForExistence(timeout: 10))
        // An empty scroll view is as narrow as its content, which wrapped this word by word.
        XCTAssertGreaterThan(note.frame.width, app.windows.firstMatch.frame.width / 2,
                             "the empty state is squeezed into a narrow column")
        XCTAssertFalse(app.staticTexts["No sessions match."].exists)
    }
}
