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

/// The owner's "Wrapping incident management with policy compliance" in shape, not words: its
/// journal's 787 events in order, as letters (`u` a prompt, `|` a turn started, `c` and `r` a
/// tool call and its result, `m` a reply, `k` reasoning, `s` a child spawned, `p` a child's
/// report, `.` and `!` a turn completed or failed; capitals for a sub-agent's), with each reply's,
/// result's and report's size. 101 blocks, from short notes to 11,000 characters of Markdown.
func leadSession(_ script: inout Script) -> [Event] {
    let order = [
        "u|crm.u|crcrm.u|m.u|crcrmcrcrmcrmcrcrcrm.u|crCRCRCRCRCRKCRCRm.KCRCRCRCRCRCRKCRKCRKM|m.u|crcrm.u|m.u|",
        "m.u|m.u|m.u|crcrmcrcrcrmcrmcrcrmcrcrcrcrcrcrcrcrcrcrcrcrm.u|crmcrcrkcsrmcsrcprcrmcrcrmcrcprcprkcsrcs",
        "rcsrcsrcsrcsrcsrcsrcsrmcprcrmcprcrcrcrmcprcrcrkcprcrcrcpprcrmcrcrpcrcrcrkcrcrcrcrkcrcrcrmcprcrcrcrcr",
        "kcrcrcrcrmcprcrcrcrcrkcsrcrcrcprkcsrcsrpcsrcrpmcrcprmcrKCRCRCRKCcrRKCRKCCRCRRcrKCRKKCRKCRCCRm.RKKCCR",
        "KCRCRCRKCRKCRCRKKCRCRKKCRCRKCRKCRKCRKCRKCRCRKCRKCRKCRCRCRKCRKCRKCRCRKCRKCKRCRKCRKCRRCRCRKCRCRKCRKCRK",
        "CKRCRKCRKCRKCRKCRKCKRCRKCRKCRKCRKKCCRRKCRKKCRCKRCRKCRKCRKKCKRCRKCCRRCKRCRKCRKKMC|RmKCRKCRKCRcMsrrKCR",
        "cKCsrRm.KCRKM|mccsrrcprcrmcprcprcprcrmcprcrcrcrcrcrkcrcrcrm.u|crm.u|crmcrcrcrm.u|crcrkcsrcrc!pu|mcrm",
        "crkcrcprm.u|csrkcrcrcrcrmcprmcrcsrcsrcsrmcrcrcrmcrcrkcprcrmcprcrcrmcprcrcrmcrcrcrcrcrm.",
    ].joined()
    let replies = [
        3998, 4577, 4263, 99, 146, 196, 4868, 4686, 7825, 1340, 6332, 4808, 3521, 1985, 5636, 152,
        145, 91, 57, 2480, 191, 167, 142, 179, 117, 117, 97, 158, 193, 126, 168, 183,
        995, 11695, 281, 10854, 324, 10229, 311, 111, 78, 3656, 2922, 187, 1202, 75, 135, 3098,
        68, 104, 148, 163, 67, 106, 135, 2664,
    ]
    let outputs = [
        1207, 106, 28983, 0, 1329, 8921, 49192, 9492, 2037, 4189, 2729, 1086, 1630, 4146, 3657, 8762,
        3572, 6777, 3853, 2796, 2164, 2267, 1617, 2310, 3612, 2700, 753, 0, 3926, 0, 8405, 0,
        231, 231, 8788, 3819, 2253, 3606, 4931, 4349, 3664, 4787, 3720, 3954, 3353, 3621, 4440, 134,
        31, 0, 4022, 18170, 65, 65, 18, 5617, 18, 891, 33, 5986, 1353, 65, 65, 65,
        65, 65, 65, 65, 65, 65, 4219, 451, 5374, 310, 3015, 217, 7914, 181, 770, 4635,
        207, 615, 281, 176, 5736, 228, 2597, 340, 1216, 5204, 2207, 1904, 344, 6023, 2485, 396,
        5983, 2911, 889, 637, 795, 725, 120, 120, 348, 5511, 250, 3340, 409, 361, 65, 18,
        18, 7214, 65, 65, 65, 2046, 1787, 1377, 1086, 11149, 19649, 7674, 1086, 14061, 19278, 23778,
        22663, 10975, 1086, 4282, 7879, 19944, 10918, 14872, 18329, 11075, 13967, 11351, 27473, 5526, 16701, 9505,
        1287, 9882, 10719, 11088, 3253, 1517, 17848, 133, 7735, 8304, 3359, 1440, 2513, 4857, 1381, 10423,
        6304, 5362, 3190, 1583, 2731, 1133, 3722, 951, 4034, 997, 4645, 5039, 4053, 12419, 715, 7761,
        265, 19203, 11835, 2420, 7308, 2691, 16613, 4205, 2312, 409, 1364, 1167, 3348, 1340, 332, 2274,
        2867, 4548, 415, 4997, 663, 1763, 1495, 1653, 2288, 2086, 3217, 892, 65, 10854, 291, 65,
        3091, 2529, 65, 18, 5387, 225, 292, 274, 6208, 214, 5807, 244, 3677, 387, 1095, 969,
        2234, 336, 279, 1178, 453, 477, 479, 169, 0, 215, 65, 18, 179, 3809, 34, 5550,
        65, 18, 18, 18, 86, 5400, 277, 65, 65, 65, 71, 77, 125, 479, 49, 3401,
        202, 4471, 720, 291, 4251, 409, 737, 1913, 1324, 405, 459, 64,
    ]
    let reports = [
        5443, 5794, 1222, 4035, 5192, 7730, 4452, 5555, 5031, 5843, 5805, 5334, 7021, 1903, 1650, 1241,
        5236, 170, 152, 6049, 5616, 52, 5359, 5195, 3244, 4301, 4076,
    ]
    /// Markdown of `size` characters: headings, paragraphs, a list and a code block.
    func prose(_ size: Int) -> String {
        let parts = [
            "## Incident policy\n\n",
            "The wrapper checks each incident against the policy before it pages anyone. ",
            "- `src/incidents/policy.rs` maps severities to the rules that apply\n",
            "```rust\nlet rule = policy.rule_for(incident.severity)?;\n```\n\n",
            "Escalation keeps the audit trail the compliance review asks for.\n\n",
        ]
        var text = ""
        var index = 0
        while text.count < size {
            text += parts[index % parts.count]
            index += 1
        }
        return String(text.prefix(size))
    }
    var events = [script.event(created())]
    var (turn, count, reply, output, report) = ("t0", 0, 0, 0, 0)
    var calls: [Bool: [String]] = [false: [], true: []]
    var children: [String] = []
    func item(_ body: ItemBody, sub: Bool) -> Event {
        count += 1
        return script.event(.itemAdded(item: Item(agentMessage: nil, parentCallId: sub ? "agent" : nil, id: "i\(count)",
                                                  turnId: turn, body: body)))
    }
    for letter in order {
        let sub = letter.isUppercase
        switch letter.lowercased() {
        case "|":
            turn = "t\(events.count)"
            events.append(script.event(.turnStarted(turnId: turn)))
        case "u": events.append(item(.userMessage(text: prose(150), attachments: []), sub: false))
        case "c":
            let event = item(.toolCall(name: "Bash", input: #"{"command":"cargo test -p incidents"}"#), sub: sub)
            calls[sub, default: []].append("i\(count)")
            events.append(event)
        case "r":
            let call = calls[sub]?.isEmpty == false ? calls[sub, default: []].removeFirst() : "none"
            events.append(item(.toolResult(callId: call, output: prose(outputs[output]), isError: false), sub: sub))
            output += 1
        case "m":
            events.append(item(.assistantMessage(text: prose(replies[reply])), sub: sub))
            reply += 1
        case "k": events.append(item(.reasoning(text: "Checking the policy."), sub: sub))
        case "s":
            children.append("01C\(children.count)")
            events.append(script.event(.childSpawned(childSessionId: children[children.count - 1], hostId: nil,
                                                     task: "Audit policy \(children.count) of the incident flow")))
        case "p":
            events.append(script.event(.childReported(childSessionId: children[report % max(children.count, 1)],
                                                      turnId: "c\(report)", summary: prose(reports[report]))))
            report += 1
        case ".": events.append(script.event(.turnCompleted(turnId: turn, usage: nil)))
        default:
            events.append(script.event(.turnFailed(turnId: turn, error: TurnError(class: .fatal, message: "The agent exited."))))
        }
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

    /// The owner's session went blank on the Mac: an animated jump through it, by heights the
    /// lazy stack estimated, ended where it had drawn no rows.
    @Test func aLongSessionKeepsItsRowsOnScreenWhereverItIsScrolled() throws {
        fleet.applyForTesting(SessionUpdate(events: leadSession(&script), streaming: []), to: script.key)
        #expect(fleet.transcript(script.key).count > 90)
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
            .environment(\.pageHeights, .constant([artifact.id: 300]))
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

struct TranscriptJumpTests {
    /// Further, a smooth scroll lands by estimated heights and can end on rows never drawn.
    @Test func onlyAJumpToABlockInViewIsSmooth() {
        #expect(TranscriptTop.smooth(to: "b", shown: ["a", "b", "c"]))
        #expect(!TranscriptTop.smooth(to: "z", shown: ["a", "b", "c"]))
        #expect(!TranscriptTop.smooth(to: "a", shown: []))
    }
}
