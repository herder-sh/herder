import XCTest

// Temporary: screenshots of the secrets sheet for the PR. Not committed.
@MainActor
final class SecretsShots: XCTestCase {
    private let app = XCUIApplication()

    func testSecretsShots() throws {
        let link = try XCTUnwrap(ProcessInfo.processInfo.environment["HERDER_VAULT_LINK"])
        app.launch()
        #if os(macOS)
        resize()
        #endif
        let add = app.buttons["Add Machine"].firstMatch
        XCTAssertTrue(add.waitForExistence(timeout: 20))
        press(add)
        let field = app.descendants(matching: .any)["pairing-link"].firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        press(field)
        field.typeText(link)
        press(app.buttons["Pair"].firstMatch)
        let done = app.buttons["Done"].firstMatch
        XCTAssertTrue(done.waitForExistence(timeout: 30))
        press(done)
        settle()

        #if os(iOS)
        app.tabBars.buttons["Machines"].tap()
        #else
        let row = app.buttons.matching(NSPredicate(format: "label == 'Vault' OR label BEGINSWITH 'Vault, '")).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10), app.debugDescription)
        row.click()
        #endif
        settle()
        let open = app.buttons["vault-secrets"].firstMatch
        XCTAssertTrue(open.waitForExistence(timeout: 20), app.debugDescription)
        #if os(iOS)
        for _ in 0..<6 where !open.isHittable { app.swipeUp() }
        #endif
        press(open)

        createSet("coingate-prod")
        addSecret("coingate-prod", "API_KEY", "CoinGate merchant API, sandbox account", "sk_live_1")
        addSecret("coingate-prod", "DB_URL", "Read-only replica for reports", "postgres://x")
        createSet("personal")
        addSecret("personal", "GITHUB_TOKEN", "Opens pull requests on herder-sh repos", "ghp_x")
        toggle("coingate-prod", "devbox")
        toggle("personal", "devbox")
        toggle("personal", "laptop")
        settle()
        scrollTop()
        shoot("secrets-list")

        // The add form, its value masked.
        press(app.buttons["secret-add-personal"].firstMatch)
        let name = app.textFields["secret-name"].firstMatch
        XCTAssertTrue(name.waitForExistence(timeout: 10))
        type(name, "NPM_TOKEN")
        type(app.textFields["secret-description"].firstMatch, "Publishes packages to npm")
        type(app.secureTextFields["secret-value"].firstMatch, "npm_s3cr3t")
        settle()
        shoot("secrets-add")
        press(app.buttons["secret-save"].firstMatch)
        XCTAssertTrue(app.descendants(matching: .any)["secret-personal-NPM_TOKEN"].firstMatch.waitForExistence(timeout: 10))
        settle()

        // The grants of a set, scrolled to.
        let grant = app.switches["secret-grant-personal-laptop"].firstMatch
        reveal(grant)
        settle()
        shoot("secrets-grants")
    }

    private func createSet(_ set: String) {
        let field = app.textFields["secret-set-name"].firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 15), app.debugDescription)
        reveal(field)
        type(field, set)
        press(app.buttons["secret-set-create"].firstMatch)
        XCTAssertTrue(app.buttons["secret-add-\(set)"].firstMatch.waitForExistence(timeout: 10))
    }

    private func addSecret(_ set: String, _ name: String, _ description: String, _ value: String) {
        let add = app.buttons["secret-add-\(set)"].firstMatch
        reveal(add)
        press(add)
        let field = app.textFields["secret-name"].firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        type(field, name)
        type(app.textFields["secret-description"].firstMatch, description)
        type(app.secureTextFields["secret-value"].firstMatch, value)
        press(app.buttons["secret-save"].firstMatch)
        XCTAssertTrue(app.descendants(matching: .any)["secret-\(set)-\(name)"].firstMatch.waitForExistence(timeout: 10))
        settle()
    }

    private func toggle(_ set: String, _ host: String) {
        let grant = app.switches["secret-grant-\(set)-\(host)"].firstMatch
        XCTAssertTrue(grant.waitForExistence(timeout: 5))
        reveal(grant)
        #if os(iOS)
        grant.switches.firstMatch.tap()
        #else
        grant.click()
        #endif
        let on = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: grant)
        wait(for: [on], timeout: 10)
        Thread.sleep(forTimeInterval: 0.5)
    }

    private func type(_ element: XCUIElement, _ text: String) {
        press(element)
        element.typeText(text)
    }

    private func press(_ element: XCUIElement) {
        #if os(macOS)
        element.click()
        #else
        element.tap()
        #endif
    }

    private func reveal(_ element: XCUIElement) {
        for step in 0..<10 where !element.isHittable {
            let down = step < 5
            #if os(iOS)
            let sheet = app.scrollViews.element(boundBy: app.scrollViews.count - 1)
            if down { sheet.swipeUp(velocity: .slow) } else { sheet.swipeDown(velocity: .slow) }
            #else
            app.sheets.scrollViews.firstMatch.scroll(byDeltaX: 0, deltaY: down ? -200 : 200)
            #endif
            Thread.sleep(forTimeInterval: 0.4)
        }
    }

    private func scrollTop() {
        #if os(macOS)
        app.sheets.scrollViews.firstMatch.scroll(byDeltaX: 0, deltaY: 3000)
        #else
        let sheet = app.scrollViews.element(boundBy: app.scrollViews.count - 1)
        for _ in 0..<3 { sheet.swipeDown(velocity: .fast) }
        #endif
        settle()
    }

    private func settle() { Thread.sleep(forTimeInterval: 1.2) }

    private func shoot(_ name: String) {
        #if os(macOS)
        let screenshot = app.windows.firstMatch.screenshot()
        #else
        let screenshot = XCUIScreen.main.screenshot()
        #endif
        let attachment = XCTAttachment(screenshot: screenshot)
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    #if os(macOS)
    private func resize() {
        let window = app.windows.firstMatch
        guard window.waitForExistence(timeout: 10) else { return }
        let bar = window.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0)).withOffset(CGVector(dx: 0, dy: 10))
        bar.press(forDuration: 0.2, thenDragTo: bar.withOffset(CGVector(dx: -window.frame.minX, dy: 40 - window.frame.minY)))
        let frame = window.frame
        let corner = window.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: frame.width - 2, dy: frame.height - 2))
        corner.press(forDuration: 0.2, thenDragTo: window.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: 1278, dy: 818)))
    }
    #endif
}
