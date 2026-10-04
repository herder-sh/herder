import XCTest

/// On iPhone a session whose turn ran tool calls wider than the phone still fits the screen, and
/// a message sent from the keyboard shows in the transcript with the keyboard put away. Opens the
/// session of `fake-host-1` that ran `fixtures/tools.jsonl`, against the fake daemons of
/// `PairingUITests`, pairing first unless an earlier test did.
final class SessionOverflowUITests: XCTestCase {
    @MainActor
    func testLongToolRowsFitTheScreenAndASentMessageStaysInView() throws {
        let app = XCUIApplication()
        app.launch()
        try app.pairUnlessPaired()

        let session = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'The ordering now keeps'")).firstMatch
        XCTAssertTrue(session.waitForExistence(timeout: 15))
        session.tap()
        let transcript = app.descendants(matching: .any)["transcript"].firstMatch
        XCTAssertTrue(transcript.waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Bash"].firstMatch.waitForExistence(timeout: 10))
        screenshot(app, "tool-rows")
        assertFitsTheScreen(app)

        let composer = app.descendants(matching: .any)["composer"].firstMatch
        composer.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5), "tapping the prompt shows no keyboard")
        composer.typeText("Keep going.")
        app.buttons["Send"].tap()

        let sent = app.staticTexts["Keep going."]
        XCTAssertTrue(sent.waitForExistence(timeout: 10), "the sent message is not in the transcript")
        XCTAssertTrue(app.keyboards.firstMatch.waitForNonExistence(timeout: 5), "sending keeps the keyboard up")
        XCTAssertTrue(app.staticTexts["Working…"].waitForExistence(timeout: 10))
        XCTAssertTrue(sent.isHittable, "the sent message is out of view while the turn runs")
        screenshot(app, "sent")
        assertFitsTheScreen(app)
    }

    /// Keeps a screenshot with the results, passed or not, for the before and after in
    /// docs/screenshots.
    @MainActor
    private func screenshot(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    /// Every text and button lies within the window's width.
    @MainActor
    private func assertFitsTheScreen(_ app: XCUIApplication, file: StaticString = #filePath, line: UInt = #line) {
        let width = app.windows.firstMatch.frame.width
        for element in app.staticTexts.allElementsBoundByIndex + app.buttons.allElementsBoundByIndex
        where element.exists && !element.frame.isEmpty {
            let frame = element.frame
            XCTAssertTrue(frame.minX >= -0.5 && frame.maxX <= width + 0.5,
                          "\"\(element.label)\" spans \(frame.minX)…\(frame.maxX), outside the screen's 0…\(width)",
                          file: file, line: line)
        }
    }
}
