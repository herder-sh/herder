#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws a bot's Instructions & memory sheet at the Mac sheet's width, for its owner and for a
/// member. With `HERDER_SCREENSHOT_DIR` set, also writes them there. Drawn in a window, as its
/// text fields are AppKit's, which `ImageRenderer` leaves out.
@MainActor struct BotKnowledgeScreenshotTests {
    @Test(arguments: [(true, "owner"), (false, "member")])
    func drawsTheSheet(editable: Bool, named: String) async throws {
        let machine = BotKnowledgeTests.Machine()
        machine.bot.instructions = """
        Answer in British English, briefly.
        Payments questions go to the **payments** project; anything about deploys to **infra**.
        """
        let now = Date.now
        func ago(_ days: Double) -> String { (now - days * 86400).ISO8601Format() }
        machine.facts = [
            BotFact(id: "f1", text: "Staging deploys go through make deploy-staging", learnedAt: ago(0.01),
                    thread: BotKnowledgeTests.thread, place: .channel(name: "infra"), by: "Ada"),
            BotFact(id: "f2", text: "Ada leads payments; refunds questions go to her", learnedAt: ago(3),
                    thread: BotKnowledgeTests.thread, place: .direct, by: "Sam"),
            BotFact(id: "f3", text: "Release notes are written in the CHANGELOG, never in the PR body", learnedAt: ago(9),
                    thread: nil, place: nil, by: nil),
        ]
        let knowledge = await machine.knowledge(editable: editable)
        let sheet = BotKnowledgeSheet(knowledge: knowledge, name: "trash-can")
            .environment(\.colorScheme, .dark).frame(width: 560, height: 680)
        let view = NSHostingView(rootView: sheet)
        view.frame = CGRect(x: 0, y: 0, width: 560, height: 680)
        let window = NSWindow(contentRect: view.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: .darkAqua)
        window.contentView = view
        for _ in 0..<5 {
            view.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        let rep = try #require(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: rep)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: "bot-knowledge-\(named).png")
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("bot-knowledge-\(named).png"))
        }
    }
}
#endif
