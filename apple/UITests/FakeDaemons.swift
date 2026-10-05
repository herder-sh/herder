import XCTest

extension XCUIApplication {
    /// The link of the fake daemons the tests run against (`fake_daemon --share` prints it; pass
    /// it to `xcodebuild test` as `TEST_RUNNER_HERDER_PAIR_LINK`).
    static func pairLink() throws -> String {
        try XCTUnwrap(ProcessInfo.processInfo.environment["HERDER_PAIR_LINK"], "needs HERDER_PAIR_LINK")
    }

    /// Pairs with the fake daemons unless an earlier test did: the link pairs once, so
    /// `PairingUITests`, which runs first, leaves the app paired, and a test run on its own pairs.
    ///
    /// The app reads its machines before its first frame, so once the tab bar shows, Home shows
    /// "Add Machine" exactly when nothing is paired; no guess at how long a cold launch takes.
    @MainActor
    func pairUnlessPaired() throws {
        XCTAssertTrue(tabBars.buttons["Home"].waitForExistence(timeout: 30), "the app did not show Home")
        let add = buttons["Add Machine"].firstMatch
        guard add.exists else { return }
        let link = try Self.pairLink()
        add.tap()
        let field = descendants(matching: .any)["pairing-link"].firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        field.tap()
        field.typeText(link)
        let pair = buttons["Pair"]
        XCTAssertTrue(pair.waitForExistence(timeout: 10))
        pair.tap()
        XCTAssertTrue(buttons["Done"].waitForExistence(timeout: 30))
        buttons["Done"].tap()
    }
}
