import XCTest

extension XCUIApplication {
    /// Pairs with the fake daemons from the link in `HERDER_PAIR_LINK`, unless an earlier test
    /// did.
    @MainActor
    func pairUnlessPaired() throws {
        guard buttons["Add Machine"].firstMatch.waitForExistence(timeout: 2) else { return }
        let link = try XCTUnwrap(ProcessInfo.processInfo.environment["HERDER_PAIR_LINK"], "needs HERDER_PAIR_LINK")
        buttons["Add Machine"].firstMatch.tap()
        let field = descendants(matching: .any)["pairing-link"].firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.tap()
        field.typeText(link)
        buttons["Pair"].tap()
        XCTAssertTrue(buttons["Done"].waitForExistence(timeout: 15))
        buttons["Done"].tap()
    }
}
