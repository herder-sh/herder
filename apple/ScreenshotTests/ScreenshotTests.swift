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
        // A profile of its own, so a run on a developer's Mac keeps off their pairings.
        app.launchEnvironment["HERDER_PROFILE"] = "screenshots"
        app.launch()
        #if os(macOS)
        resize(to: CGSize(width: 1440, height: 900))
        #endif
        try pair(link)
        #if os(macOS)
        let expand = app.buttons["Expand the sidebar"].firstMatch
        if app.windows.firstMatch.frame.width >= 1300, expand.waitForExistence(timeout: 5) { expand.click() }
        #endif

        // The Board: the approval waiting on you, then every task tree with its agents folded.
        guard wait(row("Upgrade date-fns to v4"), "board") else { return finish() }
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
            // A running turn folds its tool calls, the agents among them, under Working.
            let work = app.buttons["turn-work"].firstMatch
            if wait(work, "working") { press(work) }
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
        providers()
        finish()
    }

    /// The Providers page, adding accounts on `devbox` (a second login while the first runs,
    /// then the first finished) and the machine's settings.
    private func providers() {
        #if os(macOS)
        guard section("Providers") else { return }
        settle()
        shoot("mac-providers")
        #else
        // On the iPhone, Providers is a link at the top of Machines, not a tab.
        guard section("Machines") else { return }
        let link = app.descendants(matching: .any)["providers-link"].firstMatch
        guard wait(link, "Providers link") else { return }
        press(link)
        settle()
        shoot("ios-providers")
        #endif
        mcpServers()
        guard openAddAccount() else { return }
        shoot(platform("add-account-form"))
        #if os(iOS)
        // The concurrent logins are shot on the Mac; the phone starts one and cancels it.
        let start = app.buttons["Start Login"].firstMatch
        guard wait(start, "Start Login") else { return dismiss() }
        press(start)
        _ = wait(app.descendants(matching: .any)["login-terminal"].firstMatch, "login terminal")
        settle()
        let cancel = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Cancel Login '")).firstMatch
        if wait(cancel, "Cancel Login") {
            press(cancel)
            settle()
            shoot("ios-cancel-login")
            confirmCancel()
            if wait(app.buttons["Done"].firstMatch, "cancelled login ended") {
                settle()
                shoot("ios-cancel-login-ended")
            }
        }
        dismiss()
        return
        #else
        let start = app.buttons["Start Login"].firstMatch
        guard wait(start, "Start Login") else { return dismiss() }
        press(start)
        let terminal = app.descendants(matching: .any)["login-terminal"].firstMatch
        _ = wait(terminal, "login terminal")
        settle()
        shoot(platform("add-account-login"))
        // A second login, for another provider, while the first one waits.
        let another = app.buttons["Add Another Account"].firstMatch
        if wait(another, "Add Another Account") {
            press(another)
            let codex = app.buttons["Codex"].firstMatch
            if wait(codex, "Codex choice") { press(codex) }
            settle()
            shoot(platform("add-account-second-form"))
            press(start)
            settle()
            dismiss()
            if openAddAccount() {
                shoot(platform("add-account-running"))
                // The first login: Enter answers its prompt, and the account is added.
                let first = app.descendants(matching: .any)["running-login-claude"].firstMatch
                if wait(first, "running Claude login") {
                    press(first)
                    if wait(terminal, "login terminal again") {
                        press(terminal)
                        app.typeKey(.return, modifierFlags: [])
                        let done = app.staticTexts["Account added"].firstMatch
                        if wait(done, "Account added") {
                            settle()
                            shoot(platform("add-account-finished"))
                        }
                    }
                }
            }
        }
        dismiss()
        cancelLogin()
        let machine = app.descendants(matching: .any)["sidebar-machine-devbox"].firstMatch
        if wait(machine, "devbox in the sidebar") {
            press(machine)
            settle()
            shoot("mac-machine-settings")
            // Its MCP servers, below the accounts and CLIs.
            app.sheets.firstMatch.scrollViews.firstMatch.scroll(byDeltaX: 0, deltaY: -400)
            settle()
            shoot("mac-machine-settings-mcp")
            dismiss()
        }
        #endif
    }

    /// The MCP servers on the Providers page: the list, adding one to every machine, editing
    /// one with a header, and importing a README's JSON.
    private func mcpServers() {
        let github = app.descendants(matching: .any)["mcp-server-github"].firstMatch
        guard wait(github, "MCP servers") else { return }
        reveal(github)
        settle()
        shoot(platform("mcp-servers"))
        let add = app.buttons["Add MCP Server"].firstMatch
        if wait(add, "Add MCP Server") {
            press(add)
            type("notion", into: "mcp-name")
            let http = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'HTTP'")).firstMatch
            if wait(http, "HTTP transport") { press(http) }
            type("https://mcp.notion.com/mcp", into: "mcp-url")
            settle()
            shoot(platform("mcp-add"))
            let save = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Add to'")).firstMatch
            if wait(save, "Add to machines") { press(save) }
            settle()
        }
        let linear = app.descendants(matching: .any)["mcp-server-linear"].firstMatch
        if wait(linear, "linear server") {
            reveal(linear)
            press(linear)
            settle()
            shoot(platform("mcp-edit"))
            dismiss()
        }
        let paste = app.buttons["Paste JSON"].firstMatch
        if wait(paste, "Paste JSON") {
            reveal(paste)
            press(paste)
            type(#"{"mcpServers": {"filesystem": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/srv/shared"]}, "context7": {"type": "http", "url": "https://mcp.context7.com/mcp"}}}"#,
                 into: "mcp-json")
            settle()
            shoot(platform("mcp-import"))
            let importing = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Import 2'")).firstMatch
            if wait(importing, "Import 2 Servers") { press(importing) }
            settle()
            let imported = app.descendants(matching: .any)["mcp-server-context7"].firstMatch
            if wait(imported, "imported server") {
                reveal(imported)
                settle()
                shoot(platform("mcp-imported"))
            }
        }
    }

    #if os(macOS)
    /// Cancels the Codex login still waiting, from the list of running logins.
    private func cancelLogin() {
        guard openAddAccount() else { return }
        let cancel = app.buttons["Cancel Login codex"].firstMatch
        guard wait(cancel, "Cancel Login codex") else { return dismiss() }
        press(cancel)
        settle()
        shoot("mac-cancel-login")
        confirmCancel()
        let ended = app.buttons.matching(NSPredicate(format: "label == 'Codex codex, Ended'")).firstMatch
        if wait(ended, "cancelled login ended") {
            settle()
            shoot("mac-cancel-login-ended")
        }
        dismiss()
    }
    #endif

    /// Answers the Cancel Login confirmation.
    private func confirmCancel() {
        let confirm = app.buttons.matching(NSPredicate(format: "label == 'Cancel Login'")).firstMatch
        if wait(confirm, "Cancel Login confirmation") { press(confirm) }
    }

    /// Types `text` into the field with accessibility id `field`.
    private func type(_ text: String, into field: String) {
        let input = app.descendants(matching: .any)[field].firstMatch
        guard wait(input, field) else { return }
        press(input)
        input.typeText(text)
    }

    /// Opens Add Account and picks `devbox` when it asks for a machine.
    private func openAddAccount() -> Bool {
        let add = app.buttons.matching(NSPredicate(format: "label == 'Add Account'")).firstMatch
        guard wait(add, "Add Account") else { return false }
        press(add)
        settle()
        let sheet = app.sheets.firstMatch.exists ? app.sheets.firstMatch : app
        let devbox = sheet.buttons.matching(NSPredicate(format: "label BEGINSWITH 'devbox'")).firstMatch
        if devbox.waitForExistence(timeout: 3) {
            press(devbox)
            settle()
        }
        return true
    }

    private func platform(_ name: String) -> String {
        #if os(macOS)
        "mac-\(name)"
        #else
        "ios-\(name)"
        #endif
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

    /// Opens the session titled `title` from the Board, scrolling its row into view.
    private func open(_ title: String) -> Bool {
        _ = section("Board")
        let row = row(title)
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
    /// by its label: the name in the rail, with a count after it in the sidebar.
    private func section(_ name: String) -> Bool {
        #if os(iOS)
        // Back from a session to the list, with the edge swipe: a session hides the tab bar.
        for _ in 0..<3 where app.descendants(matching: .any)["composer"].exists {
            let edge = app.coordinate(withNormalizedOffset: CGVector(dx: 0.01, dy: 0.5))
            edge.press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.5)))
            settle()
        }
        let tab = app.tabBars.buttons[name].firstMatch
        guard wait(tab, "\(name) tab") else { return false }
        tab.tap()
        #else
        let row = app.buttons.matching(NSPredicate(
            format: "label == %@ OR label BEGINSWITH %@", name, "\(name), ")).firstMatch
        guard wait(row, "\(name) in the sidebar") else { return false }
        row.click()
        #endif
        settle()
        return true
    }

    /// A session's row: one button labelled with its state, title, age and activity.
    private func row(_ title: String) -> XCUIElement {
        app.buttons.matching(NSPredicate(format: "label CONTAINS %@", ", \(title),")).firstMatch
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
        // A sheet's close button: Escape would go to a focused terminal instead.
        let close = app.sheets.firstMatch.buttons["Close"].firstMatch
        if close.exists { close.click() } else { app.typeKey(.escape, modifierFlags: []) }
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
