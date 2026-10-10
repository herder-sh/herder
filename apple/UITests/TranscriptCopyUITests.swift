import XCTest

/// A long press on a message selects a word of it, in place, with the edit menu over it: to
/// select any part of the message, or copy all of it. Opens the session of `fake-host-1` waiting
/// on an approval, against the fake daemons of `PairingUITests`, pairing first unless an
/// earlier test did.
final class TranscriptCopyUITests: XCTestCase {
    @MainActor
    func testLongPressingAMessageSelectsTextInIt() throws {
        let app = XCUIApplication()
        app.launch()
        try app.pairUnlessPaired()

        let session = app.descendants(matching: .any)["request-session"].firstMatch
        XCTAssertTrue(session.waitForExistence(timeout: 15))
        session.tap()
        let message = app.message("Run the tests.")
        XCTAssertTrue(message.waitForExistence(timeout: 10))

        message.coordinate(withNormalizedOffset: CGVector(dx: 0.15, dy: 0.5)).press(forDuration: 1)
        XCTAssertTrue(app.menuItems["Copy"].waitForExistence(timeout: 5), "a long press selected nothing")
        XCTAssertTrue(app.menuItems["Copy Message"].exists, "the edit menu does not copy the message")
        // The selection stays in the transcript: no menu of the whole message lifted out of it.
        XCTAssertTrue(message.isHittable)
        app.menuItems["Copy Message"].tap()
        XCTAssertTrue(app.menuItems["Copy"].waitForNonExistence(timeout: 5))
    }
}
