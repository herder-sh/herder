import AppKit
import XCTest

/// The keyboard shortcuts, where only the real app shows them: a button's tooltip on hover names
/// its key, ⌘/ shows every shortcut over the window, and Settings lists them to rebind. Drives
/// the copy of the app at `HERDER_APP` on the demo fleet, as `TextSelectionUITests`; each test
/// attaches a screenshot, which CI exports.
@MainActor
final class TooltipUITests: XCTestCase {
    /// A finished demo session, whose composer shows its buttons.
    private static let idle = "Tidy the README badges"
    private var app: XCUIApplication!

    override func setUp() async throws {
        continueAfterFailure = false
        let environment = ProcessInfo.processInfo.environment
        let path = try XCTUnwrap(environment["HERDER_APP"], "needs HERDER_APP")
        app = XCUIApplication(url: URL(fileURLWithPath: path))
        app.launchEnvironment["HERDER_PROFILE"] = "ui-tests-\(UUID().uuidString)"
        app.launchEnvironment["HERDER_FAKE_FLEET"] = try XCTUnwrap(environment["HERDER_PAIR_LINK"], "needs HERDER_PAIR_LINK")
        app.launchEnvironment["HERDER_FAKE_PASSKEY"] = try XCTUnwrap(environment["HERDER_PASSKEY"], "needs HERDER_PASSKEY")
        app.launch()
    }

    override func tearDown() async throws {
        app?.terminate()
    }

    /// The owner's case: hovering the mic showed nothing, so its ⇧⌘D could not be found.
    func testButtonsWithAShortcutNameItOnHover() throws {
        open(Self.idle)
        try expectTooltip(on: app.buttons["Dictate"].firstMatch, "Dictate, on this device (⇧⌘D)", shot: "mic-tooltip")
        try expectTooltip(on: app.buttons["Find in Transcript"].firstMatch, "Find in the transcript (⌘F)", shot: "find-tooltip")
    }

    func testCommandSlashShowsEveryShortcut() {
        let sheet = app.descendants(matching: .any)["cheat-sheet"]
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 30))
        Thread.sleep(forTimeInterval: 2)
        app.typeKey("/", modifierFlags: .command)
        XCTAssertTrue(sheet.waitForExistence(timeout: 5), "⌘/ showed no cheat sheet")
        XCTAssertTrue(sheet.staticTexts["Speak to Chat"].exists)
        XCTAssertTrue(sheet.staticTexts["⌃⌥⌘D"].exists)
        shoot("cheat-sheet-app")
        app.typeKey(.escape, modifierFlags: [])
        XCTAssertTrue(sheet.waitForNonExistence(timeout: 5), "Esc left the cheat sheet open")
    }

    func testSettingsListsEveryShortcutToRebind() {
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 30))
        app.typeKey(",", modifierFlags: .command)
        XCTAssertTrue(app.buttons["Reset Dictate"].waitForExistence(timeout: 10), "⌘, showed no Shortcuts pane")
        XCTAssertTrue(app.buttons["Reset Speak to Chat"].exists)
        XCTAssertTrue(app.buttons["Reset Save As…"].exists)
        shoot("settings-shortcuts-app")
    }

    // MARK: Helpers

    /// Rests the pointer on `button` until its tooltip shows `text`.
    private func expectTooltip(on button: XCUIElement, _ text: String, shot: String) throws {
        XCTAssertTrue(button.waitForExistence(timeout: 10), "no \(text)")
        // Off it first, so the tooltip is this hover's.
        app.windows.firstMatch.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.4)).hover()
        Thread.sleep(forTimeInterval: 1)
        button.hover()
        let tag = app.helpTags.firstMatch
        let shown = tag.waitForExistence(timeout: 8)
        shoot(shot)
        XCTAssertTrue(shown, "hovering showed no tooltip for \(text)")
        let read = [tag.label, tag.title, tag.value as? String ?? ""].joined(separator: " ") + tag.debugDescription
        XCTAssertTrue(read.contains(text), "the tooltip reads \(read), not \(text)")
    }

    private func open(_ session: String) {
        let row = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", ", \(session),")).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 30), "\(session) is not on the Board")
        let transcript = app.descendants(matching: .any)["transcript"].firstMatch
        let titles = app.staticTexts.matching(NSPredicate(format: "label == %@", session))
        let onBoard = titles.count
        // The Board reorders its rows as the machines report in: click once the row stays put.
        for _ in 0..<3 where titles.count <= onBoard {
            settle(row)
            row.click()
            _ = transcript.waitForExistence(timeout: 5)
            for _ in 0..<20 where titles.count <= onBoard { Thread.sleep(forTimeInterval: 0.25) }
        }
        XCTAssertGreaterThan(titles.count, onBoard, "\(session) did not open")
    }

    private func settle(_ element: XCUIElement) {
        var last = element.frame
        for _ in 0..<10 {
            Thread.sleep(forTimeInterval: 0.5)
            if element.frame == last { return }
            last = element.frame
        }
    }

    private func shoot(_ name: String) {
        let shot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        shot.name = name
        shot.lifetime = .keepAlways
        add(shot)
    }
}
