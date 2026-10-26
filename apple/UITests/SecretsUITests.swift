import XCTest

/// Manages a vault's secrets: against the fake vault (`fake_daemon --vault` prints its link; pass
/// it to `xcodebuild test` as `TEST_RUNNER_HERDER_VAULT_LINK`), whose hosts are `devbox` and
/// `laptop`, opens its page from Machines and its Secrets tab, adds a set, adds a secret to it and grants the set
/// to a host, then goes back to Machines.
final class SecretsUITests: XCTestCase {
    @MainActor
    func testAddingASetAndASecretAndGrantingIt() throws {
        let link = try XCTUnwrap(ProcessInfo.processInfo.environment["HERDER_VAULT_LINK"], "needs HERDER_VAULT_LINK")
        let app = XCUIApplication()
        app.launch()
        try app.pairUnlessPaired()

        app.tabBars.buttons["Machines"].tap()
        let open = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'vaults.paired.'")).firstMatch
        if !open.waitForExistence(timeout: 5) {
            // Pairs with the vault, once: its code works once.
            app.navigationBars.buttons["Add Machine"].tap()
            let field = app.descendants(matching: .any)["pairing-link"].firstMatch
            XCTAssertTrue(field.waitForExistence(timeout: 10))
            field.tap()
            field.typeText(link)
            app.buttons["Pair"].tap()
            // One machine paired closes the sheet; a list of results waits for Done.
            if app.buttons["Done"].waitForExistence(timeout: 10) { app.buttons["Done"].tap() }
        }
        XCTAssertTrue(open.waitForExistence(timeout: 30), "Machines shows no vault")
        for _ in 0..<5 where !open.isHittable { app.swipeUp() }
        open.tap()
        let secrets = app.buttons["Secrets"].firstMatch
        XCTAssertTrue(secrets.waitForExistence(timeout: 15), "the vault's page has no Secrets tab")
        secrets.tap()

        // A set, named in the field under the sets.
        let name = app.textFields["secret-set-name"]
        XCTAssertTrue(name.waitForExistence(timeout: 15), "the vault's page did not list its sets")
        let set = "ci-\(Int.random(in: 1000...9999))"
        name.tap()
        name.typeText(set)
        app.buttons["secret-set-create"].tap()
        let add = app.buttons["secret-add-\(set)"]
        XCTAssertTrue(add.waitForExistence(timeout: 10), "the new set did not show")

        // A secret in it; its value goes into a secure field and never shows.
        add.tap()
        let secretName = app.textFields["secret-name"]
        XCTAssertTrue(secretName.waitForExistence(timeout: 10))
        secretName.tap()
        secretName.typeText("NPM_TOKEN")
        let description = app.textFields["secret-description"]
        description.tap()
        description.typeText("Publishes packages to npm")
        let value = app.secureTextFields["secret-value"]
        XCTAssertTrue(value.exists, "the value is not typed into a secure field")
        value.tap()
        value.typeText("npm_s3cr3t")
        app.buttons["secret-save"].tap()
        let secret = app.descendants(matching: .any)["secret-\(set)-NPM_TOKEN"].firstMatch
        XCTAssertTrue(secret.waitForExistence(timeout: 10), "the secret did not show in its set")
        XCTAssertTrue(app.staticTexts["Publishes packages to npm"].exists)
        XCTAssertFalse(app.staticTexts["npm_s3cr3t"].exists)

        // Granted to devbox: the switch stays on once the vault lists the set again.
        let grant = app.switches["secret-grant-\(set)-devbox"]
        XCTAssertTrue(grant.waitForExistence(timeout: 5))
        XCTAssertEqual(grant.value as? String, "0")
        grant.switches.firstMatch.tap()
        let granted = expectation(for: NSPredicate(format: "value == '1' AND isEnabled == true"), evaluatedWith: grant)
        wait(for: [granted], timeout: 10)
        XCTAssertFalse(app.staticTexts.matching(identifier: "secrets-error").firstMatch.exists)

        // The page was pushed on Machines: Back returns there.
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(open.waitForExistence(timeout: 5), "Back did not return to Machines")
    }
}
