#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the Mac window with the sidebar open on a fleet whose bots' people started
/// conversations from Slack: Bots' 380 pt list beside an open conversation, Bots at the window's
/// full width, and the Board, where only the bot task waiting for the user shows. With `before`,
/// the same fleet with no session saying a bot started it, as the apps listed them before Bots.
/// Attaches each; with `HERDER_SCREENSHOT_DIR` set, also writes them there.
@MainActor @Suite(.serialized) struct BotsScreenshotTests {
    @Test func drawsBotsBesideAConversation() async throws {
        let (fleet, key) = try await sampleFleet()
        try await shoot(fleet, item: .bots(nil), session: key, width: 1100, named: "bots-pane.png")
    }

    @Test func drawsBotsAtFullWidth() async throws {
        let (fleet, _) = try await sampleFleet()
        try await shoot(fleet, item: .bots(nil), session: nil, width: 1440, named: "bots-wide.png")
    }

    @Test func drawsTheBoardWithTheWaitingBotTask() async throws {
        let (fleet, _) = try await sampleFleet()
        try await shoot(fleet, item: .board, session: nil, width: 1440, named: "bots-board.png")
    }

    @Test func drawsTheBoardAndChatsAsTheyWereBefore() async throws {
        let (fleet, _) = try await sampleFleet(origins: false)
        try await shoot(fleet, item: .board, session: nil, width: 1440, named: "bots-before-board.png")
        try await shoot(fleet, item: .chats, session: nil, width: 1440, named: "bots-before-chats.png")
    }

    private func shoot(_ fleet: Fleet, item: SidebarItem, session: SessionKey?, width: CGFloat,
                       named name: String) async throws {
        UserDefaults.standard.set(false, forKey: "sidebarCollapsed")
        defer { UserDefaults.standard.removeObject(forKey: "sidebarCollapsed") }
        let view = DesktopShell(fleet: fleet, sheet: .constant(nil), item: .constant(item), pages: .constant(PageStack()),
                                session: .constant(session), draft: .constant(nil), opened: { _ in })
            .environment(\.colorScheme, .dark)
        let host = NSHostingView(rootView: view)
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(x: 0, y: 0, width: width, height: 640)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        for _ in 0..<5 {
            host.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }

    /// An owned machine with project tasks and a chat started in the app, and conversations its
    /// bots `trash-can` and `acme-ops` started for people in Slack, one waiting on an approval;
    /// with `origins` false, none says a bot started it. Returns the key of Sam's DM.
    private func sampleFleet(origins: Bool = true) async throws -> (Fleet, SessionKey) {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        let api = "github.com/acme/api"
        let web = "github.com/acme/web"
        // Title, project (`nil` for a chat), the bot's origin, and whether it waits on an approval.
        let sessions: [(String, String?, SessionOrigin?, Bool)] = [
            ("Audit webhook retries", api, nil, false),
            ("Upgrade date-fns to v4", web, nil, true),
            ("Move invoices onto the ledger", api, nil, false),
            ("What is a monad?", nil, nil, false),
            ("Why invoice exports time out", api,
             SessionOrigin(botId: "trash-can", kind: .slack, by: "Sam", place: .direct), false),
            ("Pricing page shows USD in the EU", web,
             SessionOrigin(botId: "trash-can", kind: .slack, by: "Kim", place: .channel(name: "web")), false),
            ("Partial refunds in the refund policy", nil,
             SessionOrigin(botId: "trash-can", kind: .slack, by: "Alex", place: .direct), false),
            ("pacheckink", nil,
             SessionOrigin(botId: "trash-can", kind: .slack, by: "Kim", place: .channel(name: "general")), false),
            ("Roll back the staging deploy", api,
             SessionOrigin(botId: "acme-ops", kind: .slack, by: "Sam", place: .direct), true),
        ]
        let ids = sessions.indices.map { String(format: "01K%023d", $0) }
        var studio = machine("studio", name: "studio", sessions: ids, projects: [
            Project(projectId: api, name: "api", paths: [], defaultPermissionMode: nil, defaultAccount: nil, setupCommand: nil),
            Project(projectId: web, name: "web", paths: [], defaultPermissionMode: nil, defaultAccount: nil, setupCommand: nil),
        ])
        for (index, session) in sessions.enumerated() {
            studio.sessions[index].projectId = session.1
            studio.sessions[index].chat = session.1 == nil
            if origins { studio.sessions[index].origin = session.2 }
        }
        fleet.setMachinesForTesting([studio])
        for (index, session) in sessions.enumerated() {
            var script = Script(ids[index], host: "studio")
            var events = [script.event(created()), script.event(.titleChanged(title: session.0, source: .auto))]
            if session.3 {
                events.append(script.event(.approvalRequested(approvalId: "a", turnId: "t", toolCallId: "i",
                                                              summary: "Run kubectl", routedTo: .user, reason: nil, secret: nil)))
            }
            fleet.applyForTesting(SessionUpdate(events: events, streaming: []), to: script.key)
        }
        // The lists rebuild a moment after the sessions change.
        try await Task.sleep(for: .milliseconds(300))
        return (fleet, SessionKey(hostId: "studio", sessionId: ids[4]))
    }
}
#endif
