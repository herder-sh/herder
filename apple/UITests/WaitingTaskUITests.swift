import XCTest

/// Opens the task waiting on you from its Board row, against the fake daemons of `PairingUITests`:
/// `fake-host-1` has a session waiting on an approval, answered in the session.
final class WaitingTaskUITests: XCTestCase {
    @MainActor
    func testTheTaskWaitingOnYouOpensFromItsBoardRow() throws {
        let app = XCUIApplication()
        app.launchForTests()
        try app.pairUnlessPaired()

        let task = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Waiting for you, '")).firstMatch
        XCTAssertTrue(task.waitForExistence(timeout: 15))
        XCTAssertFalse(app.buttons["Allow"].exists)
        task.tap()

        // The session, with its approval pinned above the composer.
        XCTAssertTrue(app.descendants(matching: .any)["composer"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["Allow"].exists)
    }
}
