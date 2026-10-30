import XCTest

/// Opens sessions from `herder://session` links, as a bot's "Open in herder" button does, against
/// the fake daemons of `PairingUITests` (pass the waiting session's id, the last line
/// `fake_daemon --share` prints, as `TEST_RUNNER_HERDER_WAITING_SESSION`).
final class SessionLinkUITests: XCTestCase {
    @MainActor
    func testASessionLinkOpensTheSession() throws {
        let session = try XCTUnwrap(ProcessInfo.processInfo.environment["HERDER_WAITING_SESSION"],
                                    "needs HERDER_WAITING_SESSION")
        let app = XCUIApplication()
        app.launchForTests()
        try app.pairUnlessPaired()

        app.open(try XCTUnwrap(URL(string: "herder://session?host=fake-host-1&session=\(session)")))

        // The session, with its approval pinned above the composer.
        XCTAssertTrue(app.descendants(matching: .any)["composer"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.buttons["Allow"].exists)
    }

    @MainActor
    func testALinkToAnUnpairedMachineOffersAddMachine() throws {
        let app = XCUIApplication()
        app.launchForTests()
        try app.pairUnlessPaired()

        app.open(try XCTUnwrap(URL(string: "herder://session?host=elsewhere&session=01JABC")))

        let alert = app.alerts["Machine not paired"]
        XCTAssertTrue(alert.waitForExistence(timeout: 10))
        alert.buttons["Add to Fleet"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["pairing-link"].waitForExistence(timeout: 5))
    }
}
