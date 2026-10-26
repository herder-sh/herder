import AppKit
import Foundation
import Herder
@testable import HerderKit
import Observation
import Synchronization
import SwiftUI
import Testing

/// A long session as `fake_daemon --busy` has it: `turns` turns of `rounds` rounds, each a
/// Markdown message with a code block, a command and its output.
func longSession(_ script: inout Script, turns: Int = 100, rounds: Int = 7) -> [Event] {
    func item(_ id: String, _ turn: String, _ body: ItemBody) -> Event {
        script.event(.itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: id, turnId: turn, body: body)))
    }
    var events = [script.event(created())]
    for turn in 0..<turns {
        let id = "t\(turn)"
        events.append(script.event(.turnStarted(turnId: id)))
        events.append(item("u", id, .userMessage(text: "Next step \(turn).", attachments: [])))
        for round in 0..<rounds {
            events.append(item("m\(round)", id, .assistantMessage(text: """
                Round \(round): the retry worker reads `max_attempts` from the queue config.

                - `src/jobs/retry.rs` backs off exponentially, with jitter

                ```rust
                let delay = base * 2u32.pow(attempt.min(8)) + jitter();
                ```
                """)))
            events.append(item("c\(round)", id, .toolCall(name: "Bash", input: #"{"command":"cargo test"}"#)))
            events.append(item("r\(round)", id, .toolResult(callId: "c\(round)", output: "running 24 tests\nok", isError: false)))
        }
        events.append(script.event(.turnCompleted(turnId: id, usage: nil)))
    }
    return events
}

/// The session view on a long session, in a window, scrolled as a mouse wheel scrolls it.
@MainActor
final class TranscriptScrollTests {
    let fleet: Fleet
    var script = Script("01A", host: "h")
    let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 700), styleMask: [.titled],
                          backing: .buffered, defer: false)

    init() throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CocoaError(.fileReadUnknown)
        }
        self.fleet = fleet
    }

    @Test func aLongSessionOpensAtItsEndAndFollowsWhatComes() throws {
        load()
        let scroll = try open()
        #expect(scroll.fromEnd < 1)
        #expect(scroll.top > 10_000)
        reply(turn: "n1", words: 200)
        #expect(scroll.fromEnd < 1)
    }

    @Test func aSessionOpenedAsItsHistoryLoadsStaysAtItsEnd() throws {
        show()
        load()
        let scroll = try open()
        #expect(scroll.top > 10_000)
        #expect(scroll.fromEnd < 1)
    }

    @Test func scrolledUpTheReaderKeepsTheirPlaceAsOutputComes() throws {
        load()
        let scroll = try open()
        Self.wheel(scroll, 600)
        let top = scroll.top
        #expect(abs(scroll.fromEnd - 600) < 1)
        reply(turn: "n1", words: 200)
        #expect(scroll.top == top)
        for words in stride(from: 0, to: 400, by: 40) {
            stream(turn: "n2", words: words)
        }
        #expect(scroll.top == top)
    }

    /// Drawing a page more each time the reader neared the top of what was drawn put them back
    /// at the top of it, which drew another: one scroll up went all the way to the start.
    @Test func scrollingUpMovesAsFarAsScrolled() throws {
        load()
        let scroll = try open()
        let top = scroll.top
        Self.wheel(scroll, 3000)
        #expect(abs(scroll.top - (top - 3000)) < 1)
    }

    private func load() {
        fleet.applyForTesting(SessionUpdate(events: longSession(&script), streaming: []), to: script.key)
    }

    private func show() {
        guard window.contentView?.subviews.isEmpty != false else { return }
        window.contentView = NSHostingView(rootView: NavigationStack { SessionView(fleet: fleet, key: script.key) })
        window.orderFront(nil)
    }

    /// Shows the session, if it isn't, and waits for its transcript to be laid out.
    private func open() throws -> NSScrollView {
        show()
        for _ in 0..<100 {
            Self.spin(0.02)
            if let scroll = Self.transcript(in: window.contentView) {
                Self.spin(0.2)
                return scroll
            }
        }
        throw CocoaError(.fileNoSuchFile)
    }

    private func reply(turn: String, words: Int) {
        let text = String(repeating: "word ", count: words)
        fleet.applyForTesting(SessionUpdate(events: [
            script.event(.turnStarted(turnId: turn)),
            script.event(.itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: "m", turnId: turn,
                                               body: .assistantMessage(text: text)))),
            script.event(.turnCompleted(turnId: turn, usage: nil)),
        ], streaming: []), to: script.key)
        Self.spin(0.2)
    }

    private func stream(turn: String, words: Int) {
        fleet.applyForTesting(SessionUpdate(events: [], streaming: [
            Item(agentMessage: nil, parentCallId: nil, id: "s", turnId: turn,
                 body: .assistantMessage(text: String(repeating: "word ", count: words))),
        ]), to: script.key)
        Self.spin(0.05)
    }

    /// The window's scroll view taller than itself: the transcript.
    private static func transcript(in view: NSView?) -> NSScrollView? {
        guard let view else { return nil }
        if let scroll = view as? NSScrollView, scroll.height > 2 * scroll.frame.height {
            return scroll
        }
        return view.subviews.lazy.compactMap(transcript(in:)).first
    }

    /// Scrolls as a mouse wheel does, 50 points at a time: positive `points` towards the top.
    private static func wheel(_ scroll: NSScrollView, _ points: Int) {
        for _ in 0..<abs(points) / 50 {
            let event = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1,
                                wheel1: Int32(points.signum() * 50), wheel2: 0, wheel3: 0)
            if let event = event.flatMap(NSEvent.init(cgEvent:)) { scroll.scrollWheel(with: event) }
            spin(0.016)
        }
        spin(0.2)
    }

    private static func spin(_ seconds: Double) {
        RunLoop.main.run(until: Date().addingTimeInterval(seconds))
    }
}

private extension NSScrollView {
    var height: CGFloat { documentView?.frame.height ?? 0 }
    /// Where the view is scrolled, from the transcript's top, and how far it is from the end.
    var top: CGFloat { documentVisibleRect.minY }
    var fromEnd: CGFloat { height - documentVisibleRect.maxY }
}

@MainActor
struct TranscriptCacheTests {
    /// A 2,000-item session's transcript is built on every update a running turn streams: it
    /// was quadratic, at 0.3 s a build.
    @Test func aLongSessionsTranscriptBuildsWithinBudget() throws {
        var script = Script()
        var model = SessionModel(key: script.key)
        model.apply(SessionUpdate(events: longSession(&script), streaming: []))
        #expect(model.log.count >= 2000)
        let start = ContinuousClock.now
        let blocks = Transcript.collapsed(model)
        #expect(ContinuousClock.now - start < .milliseconds(100))
        #expect(blocks.count == 300)
    }

    /// The view asks for the transcript on each redraw: it is built once per change to the
    /// session, and a view that read it redraws when the session changes.
    @Test func theTranscriptIsBuiltOncePerChange() throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        var script = Script("01A", host: "h")
        fleet.applyForTesting(SessionUpdate(events: [script.event(created())], streaming: []), to: script.key)
        let first = fleet.transcript(script.key)
        #expect(first.isEmpty)

        let invalidated = Mutex(false)
        withObservationTracking { _ = fleet.transcript(script.key) } onChange: { invalidated.withLock { $0 = true } }
        fleet.applyForTesting(SessionUpdate(events: [], streaming: [
            Item(agentMessage: nil, parentCallId: nil, id: "s", turnId: "t", body: .assistantMessage(text: "Hel")),
        ]), to: script.key)
        #expect(invalidated.withLock { $0 })
        let blocks = fleet.transcript(script.key)
        guard case .assistant(_, let text, let streaming) = blocks.first else {
            Issue.record("the transcript was not built again: \(blocks)")
            return
        }
        #expect(text == "Hel" && streaming)
    }
}

struct FoldSpotTests {
    let viewport = CGRect(x: 0, y: -600, width: 400, height: 800)

    @Test func theFoldedLineTakesTheClosingLinesPlaceOnScreen() {
        // A line 3/4 down the view, in its own space with the view's bounds converted to it.
        let line = CGRect(x: 0, y: -10, width: 80, height: 20)
        #expect(FoldSpot.anchor(of: line, in: viewport) == UnitPoint(x: 0, y: 0.75))
    }

    @Test func aLineOutOfSightLandsAtTheViewsEdge() {
        #expect(FoldSpot.anchor(of: CGRect(x: 0, y: 900, width: 80, height: 20), in: viewport).y == 1)
        #expect(FoldSpot.anchor(of: CGRect(x: 0, y: -2000, width: 80, height: 20), in: viewport).y == 0)
        #expect(FoldSpot.anchor(of: .zero, in: .zero) == .bottom)
    }
}
