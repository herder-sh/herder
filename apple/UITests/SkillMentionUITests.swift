import XCTest

/// Mentions a skill in the composer with `$`: against the fake daemons of `PairingUITests`, the
/// session of `fake-host-1` waiting on an approval has the project skill its repository checks
/// in, `deploy`. The prompt queues behind the approval, so the queue shows the text sent.
final class SkillMentionUITests: XCTestCase {
    @MainActor
    func testDollarOpensThePickerAndTheChosenSkillIsSentAsItsName() throws {
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
        let composer = app.descendants(matching: .any)["composer"].firstMatch
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        composer.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5))

        // `$` opens the picker of the session's skills.
        composer.typeText("$")
        let option = app.buttons["skill-option-deploy"]
        XCTAssertTrue(option.waitForExistence(timeout: 10), "typing $ opened no skill picker")
        // Typing narrows it; choosing completes the mention and closes it.
        composer.typeText("dep")
        XCTAssertTrue(option.waitForExistence(timeout: 2))
        option.tap()
        XCTAssertTrue(option.waitForNonExistence(timeout: 5), "the picker stayed open")

        // The chip, with the skill's description.
        let chip = app.descendants(matching: .any)["skill-chip-deploy"].firstMatch
        XCTAssertTrue(chip.waitForExistence(timeout: 5))
        XCTAssertTrue(chip.staticTexts["Deploy the app to staging."].exists)

        composer.typeText("to staging.")
        app.buttons["Send"].tap()
        // What was sent: the mention as `$name`, queued behind the approval.
        XCTAssertTrue(app.staticTexts["$deploy to staging."].waitForExistence(timeout: 10))
        XCTAssertFalse(chip.exists)
    }
}
