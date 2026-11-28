import XCTest

/// The Integrations mock (P30.1) is reached from Hub inside the demo, and nowhere else: opens the
/// catalog, a failing integration's page and the install sheet, attaching each as a screenshot
/// (`ios-integrations*.png`, which the apple workflow uploads).
final class IntegrationsDemoUITests: XCTestCase {
    @MainActor
    func testTheDemoShowsTheIntegrationsMock() throws {
        let app = XCUIApplication()
        app.launchEnvironment["HERDER_PROFILE"] = "integrations-\(UUID().uuidString)"
        app.launch()

        let explore = app.buttons["explore-demo"].firstMatch
        XCTAssertTrue(explore.waitForExistence(timeout: 15), "the empty state offers no demo")
        explore.tap()
        app.tabBars.buttons["Hub"].tap()

        let link = app.descendants(matching: .any)["integrations-link"].firstMatch
        XCTAssertTrue(link.waitForExistence(timeout: 10), "Hub has no Integrations in the demo")
        for _ in 0..<8 where !link.isHittable { app.swipeUp() }
        link.tap()

        XCTAssertTrue(app.descendants(matching: .any)["integrations-mock"].firstMatch.waitForExistence(timeout: 10),
                      "the catalog does not say it is a mock")
        shoot(app, "ios-integrations")
        app.swipeUp()
        shoot(app, "ios-integrations-available")
        app.swipeDown()

        let sentry = app.descendants(matching: .any)["integration-io.sentry.mcp"].firstMatch
        XCTAssertTrue(sentry.waitForExistence(timeout: 5), "the catalog lists no Sentry")
        sentry.tap()
        XCTAssertTrue(app.staticTexts["The token has expired"].waitForExistence(timeout: 10),
                      "Sentry's page shows no problem")
        shoot(app, "ios-integrations-page")
        app.navigationBars.buttons.firstMatch.tap()

        let install = app.descendants(matching: .any)["install-app.linear"].firstMatch
        XCTAssertTrue(install.waitForExistence(timeout: 5), "the catalog offers no Linear")
        for _ in 0..<6 where !install.isHittable { app.swipeUp() }
        install.tap()
        XCTAssertTrue(app.staticTexts["Install Linear"].waitForExistence(timeout: 10), "no install sheet")
        shoot(app, "ios-integrations-install")

        // Out of the demo, the empty state has no Hub to find it in.
        app.buttons["Close"].firstMatch.tap()
        app.buttons["leave-demo"].firstMatch.tap()
        XCTAssertTrue(app.buttons["explore-demo"].firstMatch.waitForExistence(timeout: 10))
        XCTAssertFalse(app.descendants(matching: .any)["integrations-link"].exists)
    }

    @MainActor
    private func shoot(_ app: XCUIApplication, _ name: String) {
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = name
        shot.lifetime = .keepAlways
        add(shot)
    }
}
