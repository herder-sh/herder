import XCTest

extension XCUIApplication {
    /// A transcript message's text, a text view to select in, found by what it says.
    func message(_ text: String) -> XCUIElement {
        textViews.matching(NSPredicate(format: "value == %@", text)).firstMatch
    }

    /// Opens the task waiting on you from its Board row: `fake-host-1`'s session with an approval.
    @MainActor
    func openWaitingTask() {
        let task = buttons.matching(NSPredicate(format: "label BEGINSWITH 'Waiting for you, '")).firstMatch
        XCTAssertTrue(task.waitForExistence(timeout: 15))
        task.tap()
    }

    /// The links of the fake daemons the tests run against (`fake_daemon --share` prints the
    /// first; pass it to `xcodebuild test` as `TEST_RUNNER_HERDER_PAIR_LINK`), and `extra` ones.
    static func fakeFleet(_ extra: [String] = []) throws -> String {
        let link = try XCTUnwrap(ProcessInfo.processInfo.environment["HERDER_PAIR_LINK"], "needs HERDER_PAIR_LINK")
        return ([link] + extra).joined(separator: " ")
    }

    /// Launches the app on the fake daemons, which it joins at launch unless an earlier test
    /// did (`Fleet.joinFakeFleet`), and waits for the Board.
    @MainActor
    func launchOnFakeFleet(_ extra: [String] = []) throws {
        launchEnvironment["HERDER_FAKE_FLEET"] = try Self.fakeFleet(extra)
        launch()
        XCTAssertTrue(tabBars.buttons["Board"].waitForExistence(timeout: 30), "the app did not show the Board")
    }
}
