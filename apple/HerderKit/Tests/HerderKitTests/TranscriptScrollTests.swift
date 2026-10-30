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

/// A long-running lead session as the owner's "Wrapping incident management with policy
/// compliance" is: `turns` turns of prose and tool calls, 19 children spawned and reporting, and
/// an HTML mockup published every fourth turn, the page in `pages` under its attachment id.
func leadSession(_ script: inout Script, turns: Int = 40, page: Data, pages: inout [AttachmentId: Data]) -> [Event] {
    func item(_ id: String, _ turn: String, _ body: ItemBody) -> Event {
        script.event(.itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: id, turnId: turn, body: body)))
    }
    var events = [script.event(created())]
    for turn in 0..<turns {
        let id = "t\(turn)"
        events.append(script.event(.turnStarted(turnId: id)))
        events.append(item("u", id, .userMessage(text: "Step \(turn): wrap incident management with policy compliance.",
                                                 attachments: [])))
        for round in 0..<4 {
            let words = 20 + (turn * 7 + round * 13) % 60
            events.append(item("m\(round)", id, .assistantMessage(
                text: String(repeating: "The policy check wraps the incident flow. ", count: words))))
            events.append(item("c\(round)", id, .toolCall(name: "Bash", input: #"{"command":"cargo test"}"#)))
            events.append(item("r\(round)", id, .toolResult(callId: "c\(round)", output: "ok", isError: false)))
        }
        if turn < 38, turn % 2 == 0 {
            events.append(script.event(.childSpawned(childSessionId: "01C\(turn)", hostId: nil,
                                                     task: "Audit policy \(turn) of the incident flow")))
        } else if turn < 38 {
            events.append(script.event(.childReported(childSessionId: "01C\(turn - 1)", turnId: "c1", summary: "## Policy \(turn)\n\n"
                + String(repeating: "- Checked a compliance rule and wrapped it.\n", count: 12))))
        }
        if turn % 4 == 3 {
            let attachment = Attachment(attachmentId: "01PAGE\(turn)", mediaType: "application/octet-stream",
                                        size: UInt64(page.count), name: "mockup-\(turn).html")
            events.append(script.event(.artifactPublished(title: "Mockup \(turn)", attachment: attachment,
                                                          url: "https://krowk.com/a/mockup-\(turn)", expiresAt: nil)))
            pages[attachment.attachmentId] = page
        }
        events.append(script.event(.turnCompleted(turnId: id, usage: nil)))
    }
    return events
}

/// The session view on a long session, in a window, scrolled as a mouse wheel scrolls it. Each
/// step waits for what it expects rather than for a set time, so a busy Mac only slows it; one
/// at a time, as their windows share the main loop.
@MainActor @Suite(.serialized)
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

    /// The owner's session went blank on the Mac: rows resizing as they appear (a page's card
    /// measured after it loads) under a jump left the lazy stack over rows it never drew.
    @Test func aLongSessionKeepsItsRowsOnScreenWhereverItIsScrolled() throws {
        var pages: [AttachmentId: Data] = [:]
        let events = leadSession(&script, page: try Self.mockup(), pages: &pages)
        fleet.applyForTesting(SessionUpdate(events: events, streaming: []), to: script.key)
        for (id, page) in pages { fleet.setAttachmentForTesting(id, page) }
        let scroll = try open()
        #expect(drawn(scroll))
        // To the top a wheel at a time, and back.
        while scroll.top > 0 {
            Self.wheel(scroll, 1000)
            #expect(drawn(scroll), "blank at \(scroll.top) of \(scroll.height)")
        }
        while scroll.fromEnd > 1 {
            Self.wheel(scroll, -1000)
            #expect(drawn(scroll), "blank at \(scroll.top) of \(scroll.height)")
        }
        // Dragged on the scroller, to anywhere.
        for fraction in [0.0, 0.9, 0.1, 0.5, 1.0, 0.3, 0.05, 0.99, 0.0, 0.7] {
            scroll.contentView.scroll(to: NSPoint(x: 0, y: fraction * max(scroll.height - scroll.contentView.bounds.height, 0)))
            scroll.reflectScrolledClipView(scroll.contentView)
            Self.settle(scroll)
            #expect(drawn(scroll), "blank at \(scroll.top) of \(scroll.height)")
        }
        // Resized.
        for width in [600.0, 1300, 900] {
            window.setContentSize(NSSize(width: width, height: 700))
            Self.settle(scroll)
            #expect(drawn(scroll), "blank at \(scroll.top) of \(scroll.height) at \(width) wide")
        }
    }

    /// A page's card scrolled out of the lazy stack and back is drawn again at the height its
    /// page measured, not at the placeholder's, which grew as it landed.
    @Test func aPagesCardDrawnAgainKeepsItsMeasuredHeight() throws {
        let page = Attachment(attachmentId: "01PAGE", mediaType: "application/octet-stream", size: 1, name: "page.html")
        fleet.applyForTesting(SessionUpdate(events: [
            script.event(created()),
            script.event(.artifactPublished(title: "Mockup", attachment: page, url: nil, expiresAt: nil)),
        ], streaming: []), to: script.key)
        fleet.setAttachmentForTesting("01PAGE", try Self.mockup())
        let artifact = try #require(fleet.artifacts(of: script.key).first)
        let card = ArtifactBlock(artifact: artifact, fleet: fleet, key: script.key)
            .environment(\.pageHeights, .constant(["01PAGE": 300]))
            .frame(width: 600)
        let plain = ArtifactBlock(artifact: artifact, fleet: fleet, key: script.key).frame(width: 600)
        let kept = NSHostingView(rootView: card).fittingSize.height
        let placeholder = NSHostingView(rootView: plain).fittingSize.height
        #expect(abs(kept - placeholder - (300 - 160)) < 0.5, "\(kept) against \(placeholder)")
    }

    /// Whether the transcript shows rows: what its layers draw, without laying anything out, has
    /// ink beyond the checkpoint rail at its left edge.
    private func drawn(_ scroll: NSScrollView) -> Bool {
        guard let root = window.contentView, let layer = root.layer else { return false }
        let frame = scroll.convert(scroll.bounds, to: root)
        let (width, height) = (Int(frame.width), Int(frame.height))
        guard let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                                      space: CGColorSpaceCreateDeviceRGB(),
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue),
              let data = context.data else { return false }
        context.translateBy(x: -frame.minX, y: -(root.isFlipped ? root.bounds.height - frame.maxY : frame.minY))
        layer.render(in: context)
        let pixels = data.bindMemory(to: UInt8.self, capacity: width * height * 4)
        func light(_ x: Int, _ y: Int) -> Int {
            let i = (y * width + x) * 4
            return Int(pixels[i]) + Int(pixels[i + 1]) + Int(pixels[i + 2])
        }
        let ground = light(width - 2, height / 2)
        var ink = 0
        for y in stride(from: 0, to: height, by: 4) {
            for x in stride(from: 60, to: width, by: 4) where abs(light(x, y) - ground) > 90 { ink += 1 }
        }
        // A blank transcript has none; the sparsest stretch of rows, a few hundred.
        return ink > 100
    }

    /// The demo's wide HTML mockup.
    private static func mockup() throws -> Data {
        let page = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("crates/herder-ffi/fixtures/demo/invoices-mockup.html")
        return try Data(contentsOf: page)
    }

    private func load() {
        fleet.applyForTesting(SessionUpdate(events: longSession(&script), streaming: []), to: script.key)
    }

    private func show() {
        guard window.contentView?.subviews.isEmpty != false else { return }
        window.contentView = NSHostingView(rootView: NavigationStack { SessionView(fleet: fleet, key: script.key) })
        window.orderFront(nil)
    }

    /// Shows the session, if it isn't, and waits for its transcript to be laid out at its end.
    private func open() throws -> NSScrollView {
        show()
        var found: NSScrollView?
        Self.wait { found = Self.transcript(in: self.window.contentView); return found != nil }
        guard let scroll = found else { throw CocoaError(.fileNoSuchFile) }
        Self.settle(scroll)
        return scroll
    }

    private func reply(turn: String, words: Int) {
        let text = String(repeating: "word ", count: words)
        fleet.applyForTesting(SessionUpdate(events: [
            script.event(.turnStarted(turnId: turn)),
            script.event(.itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: "m", turnId: turn,
                                               body: .assistantMessage(text: text)))),
            script.event(.turnCompleted(turnId: turn, usage: nil)),
        ], streaming: []), to: script.key)
        if let scroll = Self.transcript(in: window.contentView) { Self.settle(scroll) }
    }

    private func stream(turn: String, words: Int) {
        fleet.applyForTesting(SessionUpdate(events: [], streaming: [
            Item(agentMessage: nil, parentCallId: nil, id: "s", turnId: turn,
                 body: .assistantMessage(text: String(repeating: "word ", count: words))),
        ]), to: script.key)
        if let scroll = Self.transcript(in: window.contentView) { Self.settle(scroll) }
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
        settle(scroll)
    }

    /// Runs the main loop until `done`, for up to 10 seconds.
    private static func wait(_ done: () -> Bool) {
        let deadline = Date().addingTimeInterval(10)
        while !done() && Date() < deadline { spin(0.02) }
    }

    /// Runs the main loop until the transcript's size and position hold for a few frames.
    private static func settle(_ scroll: NSScrollView) {
        var last = (scroll.height, scroll.top)
        var still = 0
        wait {
            let now = (scroll.height, scroll.top)
            still = now == last ? still + 1 : 0
            last = now
            return still >= 5
        }
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
