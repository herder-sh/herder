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

        // The Board: every open task by day, the one waiting on you an amber dot.
        guard wait(row("Upgrade date-fns to v4"), "board") else { return finish() }
        settle()
        #if os(macOS)
        shoot("mac-home")
        // The machines footer's popover, which the window's own screenshot leaves out.
        if openMachines() {
            shoot("mac-sidebar-machines", screen: true)
            app.typeKey(.escape, modifierFlags: [])
            settle()
        }
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
                back()
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

        // A question several of whose choices may be picked, with two of them ticked.
        if open("Run checkout e2e in more browsers") {
            for label in ["Firefox", "Mobile Chrome"] {
                let choice = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", label)).firstMatch
                if wait(choice, "\(label) choice") { press(choice) }
            }
            settle()
            shoot(platform("multi-select"))
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
        skills()
        providers()
        integrations()
        archiving()
        project()
        finish()
    }

    /// Archives a finished task from the Board, which takes its row off, then undoes it: on the
    /// Mac from its circle and with ⌘Z, then with J and E and the toast's Undo; on the iPhone
    /// with a swipe and the toast's Undo.
    private func archiving() {
        let title = "Tidy the README badges"
        guard section("Board") else { return }
        let task = row(title)
        guard wait(task, "\(title) on the Board") else { return }
        reveal(task)
        #if os(macOS)
        let circle = app.buttons["Archive \(title)"].firstMatch
        guard wait(circle, "archive circle") else { return }
        press(circle)
        guard gone(task, "archived from its circle") else { return }
        app.typeKey("z", modifierFlags: .command)
        guard wait(task, "back with ⌘Z") else { return }
        settle()
        // Opening the task puts its cursor on it, and its composer takes the keyboard; a click in
        // the Board below its rows gives the keyboard back to the Board.
        press(task)
        settle()
        app.windows.firstMatch.coordinate(withNormalizedOffset: CGVector(dx: 0.29, dy: 0.95)).click()
        app.typeKey("e", modifierFlags: [])
        guard gone(task, "archived with E") else { return }
        #else
        task.swipeLeft()
        let archive = app.buttons["Archive"].firstMatch
        guard wait(archive, "swipe's Archive") else { return }
        archive.tap()
        guard gone(task, "archived with a swipe") else { return }
        #endif
        let undo = app.buttons["Undo"].firstMatch
        guard wait(undo, "toast's Undo") else { return }
        press(undo)
        _ = wait(task, "back with Undo")
    }

    /// A project's tasks, as the Board's rows, then archiving one from that list and undoing it
    /// with the toast's Undo: from its circle on the Mac, with a swipe on the iPhone.
    private func project() {
        #if os(iOS)
        guard section("Projects") else { return }
        let project = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'api'")).firstMatch
        guard wait(project, "api in Projects") else { return }
        press(project)
        #else
        guard section("api") else { return }
        #endif
        let title = "Rate-limit the public API per key"
        let task = row(title)
        guard wait(task, "\(title) in api") else { return }
        settle()
        shoot(platform("project"))
        reveal(task)
        #if os(macOS)
        let circle = app.buttons["Archive \(title)"].firstMatch
        guard wait(circle, "archive circle in api") else { return }
        press(circle)
        #else
        task.swipeLeft()
        let archive = app.buttons["Archive"].firstMatch
        guard wait(archive, "swipe's Archive in api") else { return }
        archive.tap()
        #endif
        guard gone(task, "archived from api") else { return }
        let undo = app.buttons["Undo"].firstMatch
        guard wait(undo, "toast's Undo in api") else { return }
        press(undo)
        _ = wait(task, "back in api with Undo")
    }

    /// Whether `element` goes within a few seconds; when it does not, records `what` as missed.
    private func gone(_ element: XCUIElement, _ what: String) -> Bool {
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: element)
        if XCTWaiter().wait(for: [gone], timeout: 5) == .completed { return true }
        missed.append(what)
        let tree = XCTAttachment(string: app.debugDescription)
        tree.name = "missed \(what)"
        tree.lifetime = .keepAlways
        add(tree)
        return false
    }

    /// Integrations: every machine's bots, then `trash-can`'s page, then Add Bot with its Slack
    /// app manifest. The demo's bots wait for a vault, so none reaches Slack.
    private func integrations() {
        #if os(macOS)
        guard section("Integrations") else { return }
        #else
        // On the iPhone, Integrations is a link at the top of Machines, not a tab.
        guard section("Machines") else { return }
        let link = app.descendants(matching: .any)["integrations-link"].firstMatch
        guard wait(link, "Integrations link") else { return }
        press(link)
        #endif
        let bot = app.descendants(matching: .any)["bot-trash-can"].firstMatch
        guard wait(bot, "trash-can in Integrations") else { return }
        settle()
        shoot(platform("integrations"))
        press(bot)
        guard wait(app.staticTexts["U04SAM0001"].firstMatch, "trash-can's people") else { return }
        settle()
        shoot(platform("bot"))
        back()
        let add = app.buttons["Add Bot"].firstMatch
        guard wait(add, "Add Bot") else { return }
        press(add)
        type("trip1-team\n", into: "bot-name")
        settle()
        shoot(platform("add-bot"))
        dismiss()
    }

    /// The Skills page: the library, a switch per machine, one skill off on `studio`.
    private func skills() {
        #if os(macOS)
        guard section("Skills") else { return }
        #else
        // On the iPhone, Skills is a link at the top of Machines, not a tab.
        guard section("Machines") else { return }
        let link = app.descendants(matching: .any)["skills-link"].firstMatch
        guard wait(link, "Skills link") else { return }
        press(link)
        #endif
        guard wait(app.staticTexts["review-pr"].firstMatch, "the skill library") else { return }
        settle()
        shoot(platform("skills"))
    }

    /// The Providers page, adding accounts on `devbox` (a login that runs in Activity, a second
    /// one cancelled there) and the machine's settings, then Update All.
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
        // Every machine it can be added on starts ticked; the rest say why not.
        shoot(platform("add-account-form"))
        onlyOnDevbox()
        // Logging in closes the sheet: each machine's login runs in Activity, which needs you.
        let start = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Log in on'")).firstMatch
        guard wait(start, "Log in on") else { return dismiss() }
        press(start)
        settle()
        guard openActivity() else { return }
        shoot(platform("activity-login"), screen: true)
        let login = job("job-add-claude")
        guard wait(login, "Claude login in Activity") else { return closeActivity() }
        press(login)
        let terminal = app.descendants(matching: .any)["job-terminal"].firstMatch
        guard wait(terminal, "login terminal") else { return closeActivity() }
        #if os(iOS)
        // The phone cancels it.
        cancelLogin(named: "claude")
        closeActivity()
        #else
        // The fake login: Enter answers its prompt, and the account is added.
        press(terminal)
        app.typeKey(.return, modifierFlags: [])
        // The account in the Providers page's list, behind Activity.
        let account = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Claude, devbox,'")).firstMatch
        if wait(account, "Account added") {
            settle()
            shoot("mac-activity-login-added", screen: true)
        }
        closeActivity()
        // A second login, for Codex, cancelled from its detail. Its id is `codex-2`, as devbox
        // has a `codex` account already.
        if openAddAccount() {
            let codex = app.buttons["provider-codex"].firstMatch
            if wait(codex, "Codex choice") { press(codex) }
            onlyOnDevbox()
            press(start)
            settle()
            if openActivity() {
                let second = job("job-add-codex")
                if wait(second, "Codex login in Activity") {
                    press(second)
                    cancelLogin(named: "codex-2")
                }
                closeActivity()
            }
        }
        let machine = app.descendants(matching: .any)["sidebar-machine-devbox"].firstMatch
        if openMachines(), wait(machine, "devbox in the machines popover") {
            press(machine)
            settle()
            shoot("mac-machine-settings")
            // Its MCP servers, below the accounts and CLIs, from the page's section index.
            let mcp = app.buttons["section-index-MCP servers"].firstMatch
            if wait(mcp, "MCP servers in the section index") {
                press(mcp)
                settle()
                shoot("mac-machine-settings-mcp")
            }
            // Its backup to the demo's vault: the switches, and whether it takes handoffs.
            let backup = app.buttons["section-index-Backup"].firstMatch
            if wait(backup, "Backup in the section index") {
                press(backup)
                let handoffs = app.descendants(matching: .any)["takes-handoffs"].firstMatch
                if wait(handoffs, "Takes handoffs switch") {
                    settle()
                    shoot("mac-machine-backup")
                }
            }
            back()
        }
        // laptop, where the user is a member: no account, CLI or limit to change.
        let laptop = app.descendants(matching: .any)["sidebar-machine-laptop"].firstMatch
        if openMachines(), wait(laptop, "laptop in the machines popover") {
            press(laptop)
            settle()
            shoot("mac-machine-member")
            back()
        }
        updateAll()
        #endif
    }

    #if os(macOS)
    /// Update All on the Providers page: one installer at a time per machine, so studio's Codex
    /// waits for its Claude while devbox's runs; each ends with its outcome, and the detail of
    /// Claude's shows the warning read out of its installer, then its terminal.
    private func updateAll() {
        guard section("Providers") else { return }
        let update = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Update All ('")).firstMatch
        guard wait(update, "Update All") else { return }
        press(update)
        guard openActivity() else { return }
        let queued = app.descendants(matching: .any).matching(NSPredicate(
            format: "identifier == 'job-install-codex-studio' AND label ENDSWITH ', Queued'")).firstMatch
        if wait(queued, "studio's Codex queued") { shoot("mac-activity-queued", screen: true) }
        // The demo's installers take eight seconds, so the three end within about 16; Activity's
        // chip stops counting once they have. Its popover drops out of the view tree now and then
        // while its rows' spinners turn, so the chip, in the window, says when they are done.
        guard wait(activityIdle(), "updates done", timeout: 40) else { return closeActivity() }
        let chip = app.descendants(matching: .any)["activity"].firstMatch
        if !chip.isSelected { press(chip) }
        settle()
        shoot("mac-activity-done", screen: true)
        let claude = job("job-install-claude-studio")
        guard wait(claude, "Claude on studio updated") else { return closeActivity() }
        press(claude)
        if wait(app.descendants(matching: .any)["job-terminal"].firstMatch, "update terminal") {
            settle()
            shoot("mac-activity-detail", screen: true)
        }
        closeActivity()
    }
    #endif

    /// A static text that says `text`: its label on the iPhone, its value on the Mac. Only static
    /// texts are asked: a terminal's value is all its text, too slow to compare.
    private func text(_ text: String) -> XCUIElement {
        #if os(macOS)
        app.staticTexts.matching(NSPredicate(format: "value == %@", text)).firstMatch
        #else
        app.staticTexts[text].firstMatch
        #endif
    }

    /// A job's row in Activity, by the start of its id: `job-add-claude` for any machine.
    private func job(_ id: String) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH %@", id)).firstMatch
    }

    /// Opens Activity: the header chip's popover on the Mac, the toolbar's sheet on the iPhone.
    private func openActivity() -> Bool {
        let chip = app.descendants(matching: .any)["activity"].firstMatch
        guard wait(chip, "Activity") else { return false }
        press(chip)
        settle()
        return true
    }

    #if os(macOS)
    /// Activity's chip once no job runs or needs you: it no longer counts any.
    private func activityIdle() -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "identifier == 'activity' AND label == 'Activity'"))
            .firstMatch
    }
    #endif

    private func closeActivity() {
        #if os(macOS)
        // Escape would go to a focused terminal; a click outside the popover closes it.
        app.windows.firstMatch.coordinate(withNormalizedOffset: CGVector(dx: 0.3, dy: 0.9)).click()
        #else
        app.swipeDown(velocity: .fast)
        #endif
        settle()
    }

    /// Cancels the login whose detail is open, from its Cancel Login button, and waits for it to
    /// say it stopped.
    private func cancelLogin(named id: String) {
        let cancel = app.buttons.matching(NSPredicate(format: "label == %@", "Cancel Login \(id)")).firstMatch
        guard wait(cancel, "Cancel Login \(id)") else { return }
        press(cancel)
        settle()
        shoot(platform("cancel-login"), screen: true)
        confirmCancel()
        // Its detail says so on the iPhone; on the Mac, Activity's chip stops counting it.
        #if os(macOS)
        let stopped = wait(activityIdle(), "cancelled login stopped")
        #else
        let stopped = wait(text("Stopped"), "cancelled login stopped")
        #endif
        if stopped {
            settle()
            shoot(platform("cancel-login-ended"), screen: true)
        }
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
            // Return puts the keyboard away, so the sheet holds still while the transport changes
            // and the URL field takes the next press.
            type("notion\n", into: "mcp-name")
            let http = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'HTTP'")).firstMatch
            if wait(http, "HTTP transport") { press(http) }
            settle()
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

    /// Answers the Cancel Login confirmation.
    private func confirmCancel() {
        let confirm = app.buttons.matching(NSPredicate(format: "label == 'Cancel Login'")).firstMatch
        if wait(confirm, "Cancel Login confirmation") { press(confirm) }
    }

    /// Types `text` into the field with accessibility id `field`, once it has the keyboard: a
    /// field that just appeared may still be moving into place when first pressed.
    private func type(_ text: String, into field: String) {
        let input = app.descendants(matching: .any)[field].firstMatch
        guard wait(input, field) else { return }
        press(input)
        for _ in 0..<6 where (input.value(forKey: "hasKeyboardFocus") as? Bool) != true {
            Thread.sleep(forTimeInterval: 0.5)
            press(input)
        }
        input.typeText(text)
    }

    /// Opens Add Account and picks `devbox` when it asks for a machine.
    private func openAddAccount() -> Bool {
        let add = app.buttons.matching(NSPredicate(format: "label == 'Add Account'")).firstMatch
        guard wait(add, "Add Account") else { return false }
        press(add)
        settle()
        return true
    }

    /// Unticks the Add Account sheet's other machines, so the login runs on `devbox` alone.
    private func onlyOnDevbox() {
        for name in ["studio", "laptop"] {
            let row = app.descendants(matching: .any)["account-machine-\(name)"].firstMatch
            if row.waitForExistence(timeout: 2), row.isEnabled { press(row) }
        }
        settle()
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
    private func wait(_ element: XCUIElement, _ what: String, timeout: TimeInterval = 15) -> Bool {
        if element.waitForExistence(timeout: timeout) { return true }
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

    /// Back from a page: ⌘[ on the Mac, the navigation bar's back button on the iPhone.
    private func back() {
        #if os(macOS)
        app.typeKey("[", modifierFlags: .command)
        #else
        app.navigationBars.buttons.element(boundBy: 0).tap()
        #endif
        settle()
    }

    /// Lets animations and the transcript's scroll finish.
    private func settle() {
        Thread.sleep(forTimeInterval: 1.5)
    }

    #if os(macOS)
    /// Answers macOS's "find devices on local networks" prompt, which shows over the app at some
    /// point after it first reaches a machine.
    private func allowLocalNetwork() {
        let allow = XCUIApplication(bundleIdentifier: "com.apple.UserNotificationCenter").buttons["Allow"].firstMatch
        guard allow.exists else { return }
        allow.click()
        settle()
    }

    /// Opens the sidebar's machines popover.
    private func openMachines() -> Bool {
        let footer = app.descendants(matching: .any)["sidebar-machines"].firstMatch
        guard wait(footer, "machines footer") else { return false }
        press(footer)
        settle()
        return true
    }
    #endif

    /// `screen` takes the whole screen, for what shows outside the window, such as a popover.
    private func shoot(_ name: String, screen: Bool = false) {
        #if os(macOS)
        allowLocalNetwork()
        let screenshot = screen ? XCUIScreen.main.screenshot() : app.windows.firstMatch.screenshot()
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
