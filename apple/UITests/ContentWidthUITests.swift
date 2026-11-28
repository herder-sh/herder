import XCTest

/// Nothing is wider than the screen (docs/apple-design.md §1): the demo's main screens, pages
/// and sheets at the largest text size short of the accessibility sizes, where a row of
/// buttons or a line that cannot wrap is widest. A screen fails when any text, button or field
/// on it runs past the window's left or right edge; each screen is attached as a screenshot.
final class ContentWidthUITests: XCTestCase {
    @MainActor
    func testTheDemoFitsTheScreenAtALargeTextSize() throws {
        let app = XCUIApplication()
        app.launchEnvironment["HERDER_PROFILE"] = "width-\(UUID().uuidString)"
        app.launchArguments += ["-UIPreferredContentSizeCategoryName", "UICTContentSizeCategoryXXXL"]
        app.launch()
        let screens = Screens(app: app, test: self)

        let explore = app.buttons["explore-demo"].firstMatch
        XCTAssertTrue(explore.waitForExistence(timeout: 15), "the empty state offers no demo")
        screens.check("welcome")
        explore.tap()
        XCTAssertTrue(app.buttons["leave-demo"].waitForExistence(timeout: 10), "the demo did not open")

        for tab in ["Board", "Projects", "Chats", "Hub"] {
            app.tabBars.buttons[tab].tap()
            screens.check(tab.lowercased())
        }

        // Hub's pages, and the sheets its rows open, each checked, then back to Hub.
        let rows = [
            ("machine", "identifier BEGINSWITH 'machines.' AND NOT (identifier BEGINSWITH 'machines.paired.')"),
            ("hub", "identifier BEGINSWITH 'machines.paired.'"),
            ("you", "identifier BEGINSWITH 'setup-device-'"),
            ("person", "identifier BEGINSWITH 'person-'"),
            ("skills", "identifier == 'skills-link'"),
            ("providers", "identifier == 'providers-link'"),
        ]
        for (name, predicate) in rows {
            app.tabBars.buttons["Hub"].tap()
            let row = app.buttons.matching(NSPredicate(format: predicate)).firstMatch
            guard screens.reveal(row) else {
                XCTFail("Hub has no row for the \(name) page")
                continue
            }
            row.tap()
            screens.check(name)
            screens.back()
        }

        // Hub's sheets.
        for button in ["add-machine", "add-device"] {
            app.tabBars.buttons["Hub"].tap()
            let open = app.buttons[button].firstMatch
            guard open.waitForExistence(timeout: 5), open.isEnabled else { continue }
            open.tap()
            screens.check(button)
            screens.dismissSheet()
        }

        // A session: the task waiting on you.
        app.tabBars.buttons["Board"].tap()
        let task = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Waiting for you, '")).firstMatch
        if task.waitForExistence(timeout: 10) {
            task.tap()
            screens.check("session")
            screens.back()
        }
    }
}

/// Checks and draws each screen of `ContentWidthUITests`.
@MainActor
private struct Screens {
    let app: XCUIApplication
    let test: XCTestCase

    private static let checked: Set<XCUIElement.ElementType> = [
        .staticText, .button, .textField, .secureTextField, .textView, .switch, .link, .image, .menuButton,
    ]

    /// Fails for each element on screen that runs past the window's left or right edge, and
    /// attaches the screen as `width-<screen>`.
    func check(_ screen: String, file: StaticString = #filePath, line: UInt = #line) {
        // Let a push or a sheet finish moving in.
        sleep(1)
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "width-\(screen)"
        shot.lifetime = .keepAlways
        test.add(shot)

        guard let root = try? app.snapshot() else {
            XCTFail("no snapshot of \(screen)", file: file, line: line)
            return
        }
        let window = app.windows.firstMatch.frame
        var wide: [String] = []
        visit(root) { element in
            guard Self.checked.contains(element.elementType) else { return }
            let frame = element.frame
            // Off screen altogether: a page pushed away, a row scrolled out of a list.
            guard frame.intersects(window), frame.width > 0 else { return }
            if frame.minX < window.minX - 1 || frame.maxX > window.maxX + 1 {
                let name = element.label.isEmpty ? element.identifier : element.label
                wide.append("\(element.elementType.rawValue) “\(name.prefix(60))” at x \(Int(frame.minX))…\(Int(frame.maxX))")
            }
        }
        if !wide.isEmpty {
            XCTFail("\(screen) is wider than the \(Int(window.width)) pt screen:\n" + wide.joined(separator: "\n"),
                    file: file, line: line)
        }
    }

    /// Every element from `element` down, but not inside a sideways scroll (`SidewaysScroll`),
    /// whose table or code is meant to run past the edge.
    private func visit(_ element: any XCUIElementSnapshot, _ body: (any XCUIElementSnapshot) -> Void) {
        guard element.identifier != "sideways" else { return }
        body(element)
        for child in element.children { visit(child, body) }
    }

    /// Scrolls down until `element` can be tapped.
    func reveal(_ element: XCUIElement) -> Bool {
        guard element.waitForExistence(timeout: 5) else { return false }
        for _ in 0..<8 where !element.isHittable { app.swipeUp() }
        return element.isHittable
    }

    /// Back from a pushed page, or closes the sheet a row opened instead.
    func back() {
        let back = app.navigationBars.buttons["BackButton"].firstMatch
        if back.exists, back.isHittable {
            back.tap()
            sleep(1)
        } else {
            dismissSheet()
        }
    }

    func dismissSheet() {
        let close = app.buttons.matching(NSPredicate(format: "label IN {'Close', 'Done'}")).firstMatch
        if close.exists, close.isHittable {
            close.tap()
        } else {
            app.swipeDown(velocity: .fast)
        }
        sleep(1)
    }
}
