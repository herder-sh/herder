import XCTest

/// A long press on a message selects a word of it, in place, with the edit menu over it: to
/// select any part of the message, or copy all of it. Opens the session of `fake-host-1` waiting
/// on an approval, against the fake daemons of `FakeFleetUITests`, pairing first unless an
/// earlier test did.
final class TranscriptCopyUITests: XCTestCase {
    @MainActor
    func testLongPressingAMessageSelectsTextInIt() throws {
        let app = XCUIApplication()
        try app.launchOnFakeFleet()

        app.openWaitingTask()
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

    /// A long press and Select All select the whole message, which Copy copies: pasted into the
    /// composer, it reads as the message does.
    @MainActor
    func testSelectingAllOfAMessageCopiesItWhole() throws {
        let app = XCUIApplication()
        try app.launchOnFakeFleet()

        app.openWaitingTask()
        let message = app.message("Run the tests.")
        XCTAssertTrue(message.waitForExistence(timeout: 10))

        message.coordinate(withNormalizedOffset: CGVector(dx: 0.15, dy: 0.5)).press(forDuration: 1)
        let selectAll = app.menuItems["Select All"]
        XCTAssertTrue(selectAll.waitForExistence(timeout: 5), "the edit menu cannot select the whole message")
        selectAll.tap()
        let copy = app.menuItems["Copy"]
        XCTAssertTrue(copy.waitForExistence(timeout: 5))
        copy.tap()

        let composer = app.descendants(matching: .any)["composer"].firstMatch
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        composer.press(forDuration: 1)
        let paste = app.menuItems["Paste"]
        XCTAssertTrue(paste.waitForExistence(timeout: 5), "nothing was copied")
        paste.tap()
        XCTAssertEqual(composer.value as? String, "Run the tests.")
        // Cleared, so no draft is left for the next test.
        composer.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: "Run the tests.".count))
    }
}
