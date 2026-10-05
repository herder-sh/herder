import XCTest

/// Takes the screenshots on herder.sh: pairs the app with the demo fleet whose shared link is in
/// `HERDER_PAIR_LINK` (`fake_daemon --demo` prints it; pass it to `xcodebuild test` as
/// `TEST_RUNNER_HERDER_PAIR_LINK`), then opens each screen and attaches a screenshot of it, named
/// after the image it becomes. The `screenshots` workflow exports them from the result bundle.
///
/// A screen that cannot be reached is skipped with the view tree attached, so one run shows
/// every screen that works and why the others did not; the test then fails.
@MainActor
final class ScreenshotTests: XCTestCase {
    private let app = XCUIApplication()
    private var missed: [String] = []

    func testScreenshots() throws {
        let link = try XCTUnwrap(
            ProcessInfo.processInfo.environment["HERDER_PAIR_LINK"], "needs HERDER_PAIR_LINK")
        app.launch()
        #if os(macOS)
        resize(to: CGSize(width: 1440, height: 900))
        #endif
        try pair(link)
        #if os(macOS)
        let expand = app.buttons["Expand the sidebar"].firstMatch
        if app.windows.firstMatch.frame.width >= 1300, expand.waitForExistence(timeout: 5) { expand.click() }
        #endif

        // Home: the approval waiting on you, then the work in progress.
        guard wait(text("Upgrade date-fns to v4"), "home") else { return finish() }
        settle()
        #if os(macOS)
        shoot("mac-home")
        #else
        shoot("ios-home")
        #endif

        // A session whose Claude subagents work in parallel, then one of them.
        if open("Audit webhook retries") {
            #if os(iOS)
            shoot("ios-session")
            #endif
            let agent = app.buttons.matching(
                NSPredicate(format: "label BEGINSWITH 'Open sub-agent: Review retry backoff'")).firstMatch
            if wait(agent, "subagent row") {
                press(agent)
                settle()
                #if os(macOS)
                shoot("mac-subagents")
                #endif
                dismiss()
            }
        }

        // The approval, pinned above the composer.
        if open("Upgrade date-fns to v4") {
            #if os(macOS)
            shoot("mac-approval")
            #else
            shoot("ios-approval")
            #endif
        }

        #if os(macOS)
        // Failover after a limit, a handoff to another machine, a task with its children.
        if open("Move invoices onto the ledger") { shoot("mac-failover") }
        if open("Fix the flaky checkout e2e test") { shoot("mac-handoff") }
        if open("Roll out 30-day log retention") { shoot("mac-tasks") }
        #endif

        // The fleet.
        if section("Machines") {
            settle()
            #if os(macOS)
            shoot("mac-machines")
            #else
            shoot("ios-machines")
            #endif
        }
        finish()
    }

    private func finish() {
        XCTAssertTrue(missed.isEmpty, "could not reach: \(missed.joined(separator: ", "))")
    }

    private func pair(_ link: String) throws {
        let add = app.buttons["Add Machine"].firstMatch
        XCTAssertTrue(add.waitForExistence(timeout: 15), app.debugDescription)
        press(add)
        let field = app.descendants(matching: .any)["pairing-link"].firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5), app.debugDescription)
        press(field)
        field.typeText(link)
        let pairButton = app.buttons["Pair"].firstMatch
        XCTAssertTrue(pairButton.waitForExistence(timeout: 5), app.debugDescription)
        press(pairButton)
        let done = app.buttons["Done"].firstMatch
        XCTAssertTrue(done.waitForExistence(timeout: 30), app.debugDescription)
        press(done)
    }

    /// Opens the session titled `title` from Home, scrolling its row into view.
    private func open(_ title: String) -> Bool {
        #if os(iOS)
        // Back to the list the session is in.
        let back = app.navigationBars.buttons.firstMatch
        if back.exists { back.tap() }
        #endif
        _ = section("Home")
        // A row is one button labelled with its state, title, age and activity.
        let row = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", ", \(title),")).firstMatch
        guard wait(row, title) else { return false }
        reveal(row)
        press(row)
        settle()
        return true
    }

    /// Scrolls the list on screen until `element` can be pressed: down first, then back up.
    private func reveal(_ element: XCUIElement) {
        for step in 0..<12 where !element.isHittable {
            let down = step < 4
            #if os(iOS)
            if down { app.swipeUp(velocity: .slow) } else { app.swipeDown(velocity: .slow) }
            #else
            app.scrollViews.firstMatch.scroll(byDeltaX: 0, deltaY: down ? -250 : 250)
            #endif
            Thread.sleep(forTimeInterval: 0.5)
        }
    }

    /// Switches to the section named `name`: a tab on the iPhone, a sidebar row on the Mac, found
    /// by its symbol, as the rail's labels change with what it holds.
    private func section(_ name: String) -> Bool {
        #if os(iOS)
        let tab = app.tabBars.buttons[name].firstMatch
        guard wait(tab, "\(name) tab") else { return false }
        tab.tap()
        #else
        let symbols = ["Home": "tray", "Machines": "server.rack"]
        let symbol = symbols[name] ?? name
        let row = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", symbol)).firstMatch
        guard wait(row, "\(name) in the sidebar") else { return false }
        row.click()
        #endif
        settle()
        return true
    }

    private func text(_ text: String) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@ OR value CONTAINS %@", text, text)).firstMatch
    }

    /// Whether `element` shows up; when it does not, records `what` as missed with the tree.
    private func wait(_ element: XCUIElement, _ what: String) -> Bool {
        if element.waitForExistence(timeout: 15) { return true }
        missed.append(what)
        let tree = XCTAttachment(string: app.debugDescription)
        tree.name = "missed \(what)"
        tree.lifetime = .keepAlways
        add(tree)
        return false
    }

    private func press(_ element: XCUIElement) {
        #if os(macOS)
        element.click()
        #else
        element.tap()
        #endif
    }

    /// Closes the sheet on screen.
    private func dismiss() {
        #if os(macOS)
        app.typeKey(.escape, modifierFlags: [])
        #else
        app.swipeDown(velocity: .fast)
        #endif
        settle()
    }

    /// Lets animations and the transcript's scroll finish.
    private func settle() {
        Thread.sleep(forTimeInterval: 1.5)
    }

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
    /// Drags the window's bottom-right corner until the window is `size`, or as large as the
    /// screen allows, its top-left corner first moved to the screen's.
    private func resize(to size: CGSize) {
        let window = app.windows.firstMatch
        guard window.waitForExistence(timeout: 10) else { return }
        // The top bar is where the window drags from.
        let bar = window.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0)).withOffset(CGVector(dx: 0, dy: 10))
        bar.press(forDuration: 0.2, thenDragTo: bar.withOffset(CGVector(dx: -window.frame.minX, dy: 40 - window.frame.minY)))
        let frame = window.frame
        let corner = window.coordinate(withNormalizedOffset: .zero)
            .withOffset(CGVector(dx: frame.width - 2, dy: frame.height - 2))
        corner.press(forDuration: 0.2, thenDragTo: window.coordinate(withNormalizedOffset: .zero)
            .withOffset(CGVector(dx: size.width - 2, dy: size.height - 2)))
        let info = XCTAttachment(string: "window \(frame) → \(window.frame)")
        info.name = "window"
        info.lifetime = .keepAlways
        add(info)
    }
    #endif
}
