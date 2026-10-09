import XCTest

/// Opens a session from its request card on the Board, against the fake daemons of `PairingUITests`:
/// `fake-host-1` has a session waiting on an approval.
final class RequestCardUITests: XCTestCase {
    @MainActor
    func testTappingTheSessionLineOfARequestCardOpensTheSession() throws {
        let app = XCUIApplication()
        app.launch()
        try app.pairUnlessPaired()

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
