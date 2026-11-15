#if os(iOS)
import XCTest

/// Measures the everyday use of the iPhone app beyond its launch and its scrolling, on the demo
/// fleet under load (`fake_daemon --busy`, whose link is in `HERDER_PAIR_LINK`): opening a
/// session, reading a turn as it streams, typing a prompt, and switching tabs. `scripts/perf.sh`
/// reads what each cost the main thread out of the app's log: its turns of a frame or longer.
final class UsePerfTests: PerfTestCase {
    override func setUp() async throws {
        try prepare(links: ["HERDER_PAIR_LINK"])
        try await launchOnDemoFleet()
    }

    /// Opening the busy session from the Board and going back, five times.
    func testOpenSession() {
        let session = row(Self.busy)
        reveal(session)
        measured {
            for _ in 0..<5 {
                press(session)
                XCTAssertTrue(transcript.waitForExistence(timeout: 30), "the session did not open")
                settle()
                app.navigationBars.buttons.element(boundBy: 0).tap()
                XCTAssertTrue(session.waitForExistence(timeout: 30), "the Board did not come back")
                settle()
            }
        }
    }

    /// Reading the busy session at its end, while its turn streams a round a second.
    func testStream() {
        enter(Self.busy)
        measured { Thread.sleep(forTimeInterval: 20) }
    }

    /// Typing a prompt in the busy session, while its turn streams.
    func testType() {
        enter(Self.busy)
        typePrompt()
    }

    /// Typing the same in a finished session, where nothing else moves.
    func testTypeIdle() {
        enter("Roll out 30-day log retention")
        typePrompt()
    }

    /// Going round the tabs, three times.
    func testSwitchTabs() {
        measured {
            for _ in 0..<3 {
                for name in ["Chats", "Projects", "Setup", "Board"] {
                    let tab = app.tabBars.buttons[name].firstMatch
                    XCTAssertTrue(tab.waitForExistence(timeout: 10), "no \(name) tab")
                    tab.tap()
                    Thread.sleep(forTimeInterval: 1)
                }
            }
        }
    }

    /// About 2,000 items, its last turn streaming.
    private static let busy = "A long transcript, still streaming"

    private var transcript: XCUIElement { app.descendants(matching: .any)["transcript"].firstMatch }

    private func enter(_ title: String) {
        let session = row(title)
        reveal(session)
        press(session)
        XCTAssertTrue(transcript.waitForExistence(timeout: 30), "the session did not open")
        settle()
    }

    private func typePrompt() {
        let composer = app.textViews["composer"].firstMatch
        XCTAssertTrue(composer.waitForExistence(timeout: 10), "no prompt box")
        composer.tap()
        settle()
        measured {
            for _ in 0..<4 { composer.typeText("Run the tests again and say what failed. ") }
        }
    }
}
#endif
