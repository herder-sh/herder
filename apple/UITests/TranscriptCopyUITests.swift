import XCTest

/// A long press on a message offers Copy and Select Text for all of it, not one line. Opens the
/// session of `fake-host-1` waiting on an approval, against the fake daemons of `PairingUITests`,
/// pairing first unless an earlier test did.
final class TranscriptCopyUITests: XCTestCase {
    @MainActor
    func testLongPressingAMessageSelectsItsWholeText() throws {
        let app = XCUIApplication()
        app.launch()
        try app.pairUnlessPaired()

        let session = app.descendants(matching: .any)["request-session"].firstMatch
        XCTAssertTrue(session.waitForExistence(timeout: 15))
        session.tap()
        let message = app.staticTexts["Run the tests."]
        XCTAssertTrue(message.waitForExistence(timeout: 10))

        message.press(forDuration: 1)
        XCTAssertTrue(app.buttons["Copy"].waitForExistence(timeout: 5), "the message has no Copy")
        app.buttons["Select Text"].tap()
        let text = app.textViews["selectable-text"]
        XCTAssertTrue(text.waitForExistence(timeout: 5))
        XCTAssertEqual(text.value as? String, "Run the tests.")
    }
}
