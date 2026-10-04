import XCTest

/// Opens a session from its request card on Home, against the fake daemons of `PairingUITests`:
/// `fake-host-1` has a session waiting on an approval.
final class RequestCardUITests: XCTestCase {
    @MainActor
    func testTappingTheSessionLineOfARequestCardOpensTheSession() throws {
        let app = XCUIApplication()
        app.launch()
        // The link pairs once: `PairingUITests` runs first and leaves the app paired; on its own,
        // this test pairs.
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
        XCTAssertFalse(app.descendants(matching: .any)["composer"].exists)
        session.tap()

        // The session, with its approval pinned above the composer.
        XCTAssertTrue(app.descendants(matching: .any)["composer"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["Allow"].exists)
        XCTAssertFalse(app.descendants(matching: .any)["request-session"].exists)
    }
}
