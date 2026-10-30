import AppKit
import XCTest

/// Selecting transcript text on the Mac copies what was selected, whole: a drag across a prompt
/// bubble, a drag down a reply of several paragraphs, a triple click, ⌘A. Drives the copy of the
/// app at `HERDER_APP` on the demo fleet (`fake_daemon --demo`) through `HERDER_PAIR_LINK` and
/// `HERDER_PASSKEY`; `scripts/test-mac-ui.sh` builds both and passes them on.
@MainActor
final class TextSelectionUITests: XCTestCase {
    /// The demo session with a prompt and a reply of several paragraphs.
    private static let session = "Fix the flaky checkout e2e test"
    private static let prompt = "The checkout e2e test fails about one CI run in five. Find out why."
    private static let reply = "Reproduced on this Mac: WebKit failed 4 runs out of 20"
    /// A finished demo session, to send a prompt to.
    private static let idle = "Tidy the README badges"
    /// The prompt the owner could not select.
    private static let owners = "we are not merging smoking room to non-smoking room"

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

    /// The owner's case: a prompt with a picture in it drew each word as a text of its own, so a
    /// drag across it selected one word.
    func testDraggingAcrossAPromptWithAPictureCopiesAllOfIt() throws {
        open(Self.idle)
        let composer = app.descendants(matching: .any)["composer"].firstMatch
        XCTAssertTrue(composer.waitForExistence(timeout: 15))
        composer.click()
        let picture = NSImage(size: NSSize(width: 64, height: 48), flipped: false) { rect in
            NSColor.systemTeal.setFill()
            rect.fill()
            return true
        }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.writeObjects([picture])
        app.typeKey("v", modifierFlags: .command)
        composer.typeText(" " + Self.owners)
        app.typeKey(.return, modifierFlags: [])

        let prompt = message(containing: Self.owners)
        drag(across: prompt)
        XCTAssertEqual(copied(), "[Image #1] " + Self.owners)
    }

    func testDraggingAcrossAPromptCopiesAllOfIt() throws {
        open(Self.session)
        let prompt = message(Self.prompt)
        drag(across: prompt)
        XCTAssertEqual(copied(), Self.prompt)
    }

    func testDraggingDownAReplyCopiesAllOfIt() throws {
        open(Self.session)
        let reply = message(beginning: Self.reply)
        let text = try XCTUnwrap(reply.value as? String)
        drag(across: reply)
        XCTAssertEqual(copied(), text)
    }

    func testTripleClickCopiesTheParagraph() throws {
        open(Self.session)
        let prompt = message(Self.prompt)
        tripleClick(prompt)
        XCTAssertEqual(copied(), Self.prompt)
    }

    func testSelectAllCopiesTheWholeMessage() throws {
        open(Self.session)
        let reply = message(beginning: Self.reply)
        let text = try XCTUnwrap(reply.value as? String)
        reply.coordinate(withNormalizedOffset: CGVector(dx: 0.3, dy: 0.2)).click()
        app.typeKey("a", modifierFlags: .command)
        XCTAssertEqual(copied(), text)
    }

    // MARK: Helpers

    private func open(_ session: String) {
        let row = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", ", \(session),")).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 30), "\(session) is not on the Board")
        row.click()
        XCTAssertTrue(app.descendants(matching: .any)["transcript"].waitForExistence(timeout: 15))
    }

    private func message(_ text: String) -> XCUIElement {
        let element = app.textViews.matching(NSPredicate(format: "value == %@", text)).firstMatch
        XCTAssertTrue(element.waitForExistence(timeout: 15), "no message reads \(text)")
        return element
    }

    private func message(containing text: String) -> XCUIElement {
        let element = app.textViews.matching(NSPredicate(format: "value CONTAINS %@", text)).firstMatch
        XCTAssertTrue(element.waitForExistence(timeout: 15), "no message says \(text)")
        return element
    }

    private func message(beginning text: String) -> XCUIElement {
        let element = app.textViews.matching(NSPredicate(format: "value BEGINSWITH %@", text)).firstMatch
        XCTAssertTrue(element.waitForExistence(timeout: 15), "no message begins \(text)")
        return element
    }

    /// Presses just left of `element`'s first letter and drags to just past its last one, as a
    /// person sweeping a message does.
    private func drag(across element: XCUIElement) {
        let start = element.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: 1, dy: 6))
        let end = element.coordinate(withNormalizedOffset: CGVector(dx: 1, dy: 1)).withOffset(CGVector(dx: -1, dy: -4))
        start.press(forDuration: 0.2, thenDragTo: end)
    }

    private func tripleClick(_ element: XCUIElement) {
        let point = element.coordinate(withNormalizedOffset: CGVector(dx: 0.3, dy: 0.3))
        point.click()
        point.doubleClick()
    }

    /// ⌘C, then what is on the pasteboard.
    private func copied() -> String? {
        NSPasteboard.general.clearContents()
        app.typeKey("c", modifierFlags: .command)
        for _ in 0..<20 {
            if let string = NSPasteboard.general.string(forType: .string) { return string }
            Thread.sleep(forTimeInterval: 0.1)
        }
        return nil
    }
}
