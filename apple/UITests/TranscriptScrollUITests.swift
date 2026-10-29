import XCTest

/// A long session that is still streaming opens at its end and follows it; scrolled up, what
/// comes keeps the reader's place behind a New pill, which takes them back to the end. Against
/// `fake_daemon --busy`, whose link a profile of its own joins: pass it to `xcodebuild
/// test` as `TEST_RUNNER_HERDER_BUSY_LINK`. Skipped without it.
final class TranscriptScrollUITests: XCTestCase {
    @MainActor
    func testALongSessionOpensAtItsEndAndKeepsTheReadersPlace() throws {
        guard let link = ProcessInfo.processInfo.environment["HERDER_BUSY_LINK"] else {
            throw XCTSkip("needs HERDER_BUSY_LINK")
        }
        let app = XCUIApplication()
        app.launchEnvironment["HERDER_PROFILE"] = "transcript-scroll-\(UUID().uuidString)"
        app.launchEnvironment["HERDER_FAKE_FLEET"] = link
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Board"].waitForExistence(timeout: 30))

        let session = app.buttons.matching(NSPredicate(format: "label CONTAINS 'A long transcript, still streaming'")).firstMatch
        XCTAssertTrue(session.waitForExistence(timeout: 30))
        session.tap()
        let transcript = app.descendants(matching: .any)["transcript"].firstMatch
        let composer = app.descendants(matching: .any)["composer"].firstMatch
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        let pill = app.buttons["transcript-new"]

        // At its end, following each round as it comes.
        let rounds = app.textViews.matching(NSPredicate(format: "value BEGINSWITH 'Round '"))
        XCTAssertTrue(rounds.firstMatch.waitForExistence(timeout: 10))
        for _ in 0..<3 {
            sleep(1)
            let last = rounds.element(boundBy: rounds.count - 1)
            XCTAssertTrue(last.isHittable, "the end of the transcript is out of view")
            XCTAssertLessThanOrEqual(last.frame.maxY, composer.frame.minY + 1)
            XCTAssertFalse(pill.exists)
        }

        // Scrolled up, the rounds that come leave the reader where they were.
        transcript.swipeDown(velocity: .slow)
        let read = rounds.allElementsBoundByIndex.first { $0.isHittable }
        let reading = try XCTUnwrap(read, "nothing to read after scrolling up")
        let frame = reading.frame
        XCTAssertTrue(pill.waitForExistence(timeout: 5), "no New pill as rounds came")
        sleep(2)
        XCTAssertEqual(reading.frame, frame, "the transcript moved under the reader")

        // The pill goes back to the end, which follows again.
        pill.tap()
        XCTAssertTrue(pill.waitForNonExistence(timeout: 5))
        sleep(1)
        XCTAssertTrue(rounds.element(boundBy: rounds.count - 1).isHittable)

        // Sending at the end stays at the end: the keyboard and the queued prompt leave the
        // transcript little room, but nothing is left unseen below.
        composer.tap()
        composer.typeText("Keep the retries capped.")
        app.buttons["Send"].tap()
        sleep(2)
        XCTAssertFalse(pill.exists)
    }
}
