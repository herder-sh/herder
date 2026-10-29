import XCTest

/// A tap on inline code in a message copies exactly its text and shows "Copied" over it. Sends a
/// message with a code span in a new chat, against the fake daemons of `FakeFleetUITests`, pairing
/// first unless an earlier test did; it starts a chat, so it runs after `TabsUITests`, which
/// wants none.
final class TranscriptInlineCodeUITests: XCTestCase {
    @MainActor
    func testTappingInlineCodeCopiesIt() throws {
        let app = XCUIApplication()
        try app.launchOnFakeFleet()

        app.tabBars.buttons["Chats"].tap()
        app.navigationBars["Chats"].buttons["New Chat"].tap()
        let composer = app.textViews["composer"].firstMatch
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        composer.tap()
        composer.typeText("Set `https://example.com/callback` as the redirect URL.")
        app.buttons["Send"].tap()

        let span = app.links["https://example.com/callback"].firstMatch
        XCTAssertTrue(span.waitForExistence(timeout: 15), "the message shows no code span to tap")
        span.tap()
        XCTAssertTrue(app.staticTexts["Copied"].waitForExistence(timeout: 5), "the tap showed no Copied")
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "Copied"
        shot.lifetime = .keepAlways
        add(shot)
        // The runner may not read the app's pasteboard; the app's own composer pastes it.
        composer.tap()
        composer.press(forDuration: 1)
        let paste = app.menuItems["Paste"]
        XCTAssertTrue(paste.waitForExistence(timeout: 5), "the composer offers nothing to paste")
        paste.tap()
        XCTAssertEqual(composer.value as? String, "https://example.com/callback")
    }
}
