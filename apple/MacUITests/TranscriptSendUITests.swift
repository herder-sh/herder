import XCTest

/// Sending a message in a long session leaves its transcript showing its end, at once and
/// without a scroll: the owner's transcript went blank after a send, its text back, at the end,
/// only once they scrolled. Each shape a send takes is tried a few times in a row: a prompt
/// queued behind a running turn (it moves from the transcript to the queue tray), one delivered
/// at once, one sent after scrolling up, a long one whose composer box collapses after the send,
/// and one whose turn streams rounds of work and folds them as it ends. Drives the copy of the
/// app at `HERDER_APP` on the busy demo fleet (`fake_daemon --busy`) through `HERDER_PAIR_LINK`
/// and `HERDER_PASSKEY`, as `scripts/test-mac-ui.sh` passes them on.
@MainActor
final class TranscriptSendUITests: XCTestCase {
    /// The busy demo session: a hundred turns, the last one streaming a round every second.
    private static let session = "A long transcript, still streaming"
    /// The busy session's script for a turn of seven rounds of work, folded as it ends.
    private static let nextStep = "Next step."

    private var app: XCUIApplication!

    override func setUp() async throws {
        // Every shape is tried, to tell which ones go blank.
        continueAfterFailure = true
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

    func testSendingKeepsTheEndOfALongTranscriptInView() throws {
        try open(Self.session)
        // Opened on a fresh profile, the session shows its latest events while its journal
        // replays: the first send comes during that.
        XCTAssertTrue(rounds.firstMatch.waitForExistence(timeout: 30), "no rounds in the busy session")

        // A turn runs: each prompt queues behind it, shown in the transcript, then in the tray.
        for i in 1...3 {
            send(tagged("queued \(i)"))
            expectEnd("queued at the end, send \(i)", newest: lastRound)
        }
        for i in 1...2 {
            scrollUp()
            send(tagged("queued after scrolling up \(i)"))
            expectEnd("queued after scrolling up, send \(i)", newest: lastRound)
        }

        // Interrupted, the session runs what was queued and goes idle: each prompt is delivered
        // and answered at once.
        let interrupt = app.buttons["Interrupt"].firstMatch
        XCTAssertTrue(interrupt.waitForExistence(timeout: 10), "no Interrupt button while the turn runs")
        interrupt.click()
        XCTAssertTrue(interrupt.waitForNonExistence(timeout: 30), "the session did not go idle")

        for i in 1...3 {
            let text = tagged("delivered \(i)")
            send(text)
            expectEnd("delivered at the end, send \(i)", newest: message(text))
        }
        for i in 1...3 {
            scrollUp()
            let text = tagged("delivered after scrolling up \(i)")
            send(text)
            expectEnd("delivered after scrolling up, send \(i)", newest: message(text))
        }
        for i in 1...2 {
            // Many lines in the box, which collapses as it sends.
            let text = tagged("long \(i)") + " " + String(repeating: "Cap the retries and persist before acking. ", count: 20)
            send(text)
            expectEnd("a long prompt, send \(i)", newest: message(text))
        }
        for i in 1...3 {
            send(Self.nextStep)
            expectEnd("a turn of work at the end, send \(i)", newest: lastRound, settle: true)
        }
        for i in 1...2 {
            scrollUp()
            send(Self.nextStep)
            expectEnd("a turn of work after scrolling up, send \(i)", newest: lastRound, settle: true)
        }
    }

    // MARK: Helpers

    private var transcript: XCUIElement { app.descendants(matching: .any)["transcript"].firstMatch }
    private var composer: XCUIElement { app.descendants(matching: .any)["composer"].firstMatch }
    private var rounds: XCUIElementQuery { app.textViews.matching(NSPredicate(format: "value BEGINSWITH 'Round '")) }

    /// The newest round of work the transcript holds.
    private func lastRound() -> XCUIElement {
        rounds.element(boundBy: max(rounds.count - 1, 0))
    }

    /// The message that says `text`.
    private func message(_ text: String) -> () -> XCUIElement {
        { [app] in app!.textViews.matching(NSPredicate(format: "value == %@", text)).firstMatch }
    }

    /// `text` with a word of this run's own, so the transcript's copies from earlier runs and
    /// sends don't answer for it.
    private func tagged(_ text: String) -> String {
        "\(text) \(UUID().uuidString.prefix(8).lowercased())"
    }

    private func open(_ session: String) throws {
        let row = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", ", \(session),")).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 30), "\(session) is not on the Board")
        let titles = app.staticTexts.matching(NSPredicate(format: "label == %@", session))
        let onBoard = titles.count
        // The Board reorders its rows as the machines report: click once the row stays put.
        for _ in 0..<3 where titles.count <= onBoard {
            settle(row)
            row.click()
            _ = transcript.waitForExistence(timeout: 5)
            for _ in 0..<20 where titles.count <= onBoard { Thread.sleep(forTimeInterval: 0.25) }
        }
        try XCTSkipUnless(titles.count > onBoard, "\(session) did not open")
        XCTAssertTrue(composer.waitForExistence(timeout: 15))
    }

    /// Waits, up to five seconds, until `element` stays where it is for half a second.
    private func settle(_ element: XCUIElement) {
        var last = element.frame
        for _ in 0..<10 {
            Thread.sleep(forTimeInterval: 0.5)
            let now = element.frame
            if now == last { return }
            last = now
        }
    }

    private func send(_ text: String) {
        composer.click()
        composer.typeText(text)
        app.typeKey(.return, modifierFlags: [])
    }

    /// Scrolls the transcript a long way up, as a reader going back over the session does.
    private func scrollUp() {
        for _ in 0..<4 { transcript.scroll(byDeltaX: 0, deltaY: 2000) }
        Thread.sleep(forTimeInterval: 0.5)
    }

    /// Fails unless `newest` shows in the transcript, above the composer, within three seconds
    /// of the send (or, with `settle`, of the turn that send started ending), without a scroll.
    /// On a failure, scrolls a little, as the owner did, and says whether that brought it back.
    private func expectEnd(_ shape: String, newest: () -> XCUIElement, settle: Bool = false) {
        if settle {
            // The turn's rounds stream and fold within moments; its Interrupt button goes with it.
            _ = app.buttons["Interrupt"].firstMatch.waitForNonExistence(timeout: 15)
        }
        if shows(newest, within: 3) {
            // The first send of each shape, to show what the reader saw.
            if shape.hasSuffix("send 1") { attachScreenshot(shape) }
            return
        }
        let inView = app.textViews.allElementsBoundByIndex.filter { $0.isHittable && transcript.frame.intersects($0.frame) }
        attachScreenshot("\(shape), blank")
        transcript.scroll(byDeltaX: 0, deltaY: -10)
        let afterScroll = shows(newest, within: 2)
        XCTFail("""
            \(shape): the end of the transcript is not in view 3 s after the send \
            (\(inView.isEmpty ? "blank: no text in view" : "\(inView.count) texts in view, not the newest")); \
            \(afterScroll ? "it shows after a scroll" : "not even after a scroll")
            """)
        // The next send starts from the end, as after the owner's scroll.
        for _ in 0..<4 { transcript.scroll(byDeltaX: 0, deltaY: -5000) }
        Thread.sleep(forTimeInterval: 0.5)
    }

    private func attachScreenshot(_ name: String) {
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = name
        screenshot.lifetime = .keepAlways
        add(screenshot)
    }

    private func shows(_ element: () -> XCUIElement, within seconds: TimeInterval) -> Bool {
        let deadline = Date().addingTimeInterval(seconds)
        repeat {
            let element = element()
            if element.exists && element.isHittable && element.frame.minY < composer.frame.minY
                && element.frame.maxY > transcript.frame.minY {
                return true
            }
            Thread.sleep(forTimeInterval: 0.1)
        } while Date() < deadline
        return false
    }
}
