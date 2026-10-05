import XCTest

/// On iPhone a session opens on its transcript, not the keyboard, and dragging the transcript
/// puts the keyboard away. Opens the session of `fake-host-1` waiting on an approval, against the
/// fake daemons of `PairingUITests`, pairing first unless an earlier test did.
final class SessionKeyboardUITests: XCTestCase {
    @MainActor
    func testSessionOpensWithoutTheKeyboardAndDraggingTheTranscriptDismissesIt() throws {
        let app = XCUIApplication()
        app.launch()
        try app.pairUnlessPaired()

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
        // Down through the keyboard, as a thumb would: it follows the drag and goes. The drag
        // starts in the transcript's margin, halfway down what shows of it: the transcript runs
        // up under the navigation bar, so a point by its own height can land on the bar.
        let frame = transcript.frame
        let top = max(frame.minY, app.navigationBars.firstMatch.frame.maxY)
        app.coordinate(withNormalizedOffset: .zero)
            .withOffset(CGVector(dx: frame.minX + 8, dy: (top + frame.maxY) / 2))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.98)))
        XCTAssertTrue(app.keyboards.firstMatch.waitForNonExistence(timeout: 5), "dragging the transcript keeps the keyboard")
    }
}
