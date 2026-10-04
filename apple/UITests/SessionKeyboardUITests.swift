import XCTest

/// On iPhone a session opens on its transcript, not the keyboard, and dragging the transcript
/// puts the keyboard away. Opens the session of `fake-host-1` waiting on an approval, against the
/// fake daemons of `PairingUITests`, pairing first unless an earlier test did.
final class SessionKeyboardUITests: XCTestCase {
    @MainActor
    func testSessionOpensWithoutTheKeyboardAndDraggingTheTranscriptDismissesIt() throws {
        let app = XCUIApplication()
        app.launch()
        if app.buttons["Add Machine"].firstMatch.waitForExistence(timeout: 2) {
            let link = try XCTUnwrap(
                ProcessInfo.processInfo.environment["HERDER_PAIR_LINK"], "needs HERDER_PAIR_LINK")
            app.buttons["Add Machine"].firstMatch.tap()
            let field = app.descendants(matching: .any)["pairing-link"].firstMatch
            XCTAssertTrue(field.waitForExistence(timeout: 5))
            field.tap()
            field.typeText(link)
            app.buttons["Pair"].tap()
            XCTAssertTrue(app.buttons["Done"].waitForExistence(timeout: 15))
            app.buttons["Done"].tap()
        }

        let session = app.descendants(matching: .any)["request-session"].firstMatch
        XCTAssertTrue(session.waitForExistence(timeout: 15))
        session.tap()
        // With the approval pinned, the transcript still shows.
        let transcript = app.descendants(matching: .any)["transcript"].firstMatch
        XCTAssertTrue(transcript.waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["Allow"].exists)
        XCTAssertTrue(app.staticTexts["Run the tests."].isHittable)
        XCTAssertFalse(app.keyboards.firstMatch.waitForExistence(timeout: 2), "the keyboard opened with the session")

        app.descendants(matching: .any)["composer"].firstMatch.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5), "tapping the prompt shows no keyboard")
        // Down through the keyboard, as a thumb would: it follows the drag and goes.
        transcript.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.3))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.98)))
        XCTAssertTrue(app.keyboards.firstMatch.waitForNonExistence(timeout: 5), "dragging the transcript keeps the keyboard")
    }
}
