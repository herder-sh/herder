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

    /// Taps `add` until the pairing sheet's link field shows, and returns the field. On a cold
    /// launch the splash still fades over the Board when its button first shows, and takes a tap
    /// that lands then.
    @MainActor
    func openPairing(_ add: XCUIElement) -> XCUIElement {
        let field = descendants(matching: .any)["pairing-link"].firstMatch
        for _ in 0..<3 where !field.exists {
            add.tap()
            _ = field.waitForExistence(timeout: 5)
        }
        XCTAssertTrue(field.exists, "Add to Fleet did not open the pairing sheet")
        return field
    }

    /// The link of the fake daemons the tests run against (`fake_daemon --share` prints it; pass
    /// it to `xcodebuild test` as `TEST_RUNNER_HERDER_PAIR_LINK`).
    static func pairLink() throws -> String {
        try XCTUnwrap(ProcessInfo.processInfo.environment["HERDER_PAIR_LINK"], "needs HERDER_PAIR_LINK")
    }

    /// Pairs with the fake daemons unless an earlier test did: the link pairs once, so
    /// `PairingUITests`, which runs first, leaves the app paired, and a test run on its own pairs.
    ///
    /// The app reads its machines before its first frame, so once the tab bar shows, the Board shows
    /// "Add to Fleet" exactly when nothing is paired; no guess at how long a cold launch takes.
    @MainActor
    func pairUnlessPaired() throws {
        XCTAssertTrue(tabBars.buttons["Board"].waitForExistence(timeout: 30), "the app did not show the Board")
        let add = buttons["add-to-fleet"].firstMatch
        guard add.exists else { return }
        let link = try Self.pairLink()
        let field = openPairing(add)
        field.tap()
        field.typeText(link)
        let pair = buttons["Pair"]
        XCTAssertTrue(pair.waitForExistence(timeout: 10))
        pair.tap()
        XCTAssertTrue(buttons["Done"].waitForExistence(timeout: 30))
        buttons["Done"].tap()
    }
}
