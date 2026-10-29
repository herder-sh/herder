#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the dark theme's layers where they meet: the Mac window with the Board's 380 pt list
/// beside an open session, and the `trash-can` bot's New threads sheet. Attaches each; with
/// `HERDER_SCREENSHOT_DIR` set, also writes them there as `elevation-board.png` and
/// `elevation-new-threads.png`.
@MainActor @Suite(.serialized) struct ElevationScreenshotTests {
    @Test func drawsTheBoardBesideASession() async throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        let api = "github.com/acme/api"
        let titles = ["Audit webhook retries", "Upgrade date-fns to v4", "Move invoices onto the ledger"]
        let ids = titles.indices.map { String(format: "01K%023d", $0) }
        var studio = machine("studio", name: "studio", sessions: ids, projects: [
            Project(projectId: api, name: "api", paths: [], defaultPermissionMode: nil, defaultAccount: nil, setupCommand: nil),
        ])
        for index in ids.indices { studio.sessions[index].projectId = api }
        fleet.setMachinesForTesting([studio])
        for (index, title) in titles.enumerated() {
            var script = Script(ids[index], host: "studio")
            var events = [script.event(created()), script.event(.titleChanged(title: title, source: .auto))]
            if index == 0 {
                events += [
                    script.event(.turnStarted(turnId: "t1")),
                    script.event(.itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: "u1", turnId: "t1",
                                                       body: .userMessage(text: "Why do webhook retries pile up after a deploy?", attachments: [])))),
                    script.event(.itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: "a1", turnId: "t1",
                                                       body: .assistantMessage(text: "Retries back off from the first attempt, not the last, so a deploy's queue all fires at once. `RetryPolicy.next` should take the last attempt's time.")))),
                    script.event(.turnCompleted(turnId: "t1", usage: nil)),
                ]
            }
            fleet.applyForTesting(SessionUpdate(events: events, streaming: []), to: script.key)
        }
        try await Task.sleep(for: .milliseconds(300))
        UserDefaults.standard.set(false, forKey: "sidebarCollapsed")
        defer { UserDefaults.standard.removeObject(forKey: "sidebarCollapsed") }
        let view = DesktopShell(fleet: fleet, sheet: .constant(nil), item: .constant(.board), pages: .constant(PageStack()),
                                session: .constant(SessionKey(hostId: "studio", sessionId: ids[0])), draft: .constant(nil),
                                opened: { _ in })
        try await snap(view, size: CGSize(width: 1280, height: 720), named: "elevation-board.png")
    }

    @Test func drawsTheNewThreadsSheet() async throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        let api = "github.com/acme/api"
        let studio = machine("studio", name: "studio", sessions: [], projects: [
            Project(projectId: api, name: "api", paths: [], defaultPermissionMode: nil, defaultAccount: nil, setupCommand: nil),
        ], accounts: [
            Account(accountId: "claude", provider: "claude", label: "claude", configDir: nil, email: "sam@acme.dev",
                    usage: [], fallback: false, needsLogin: nil),
        ])
        fleet.setMachinesForTesting([studio])
        let bot = Bot(
            botId: "trash-can", kind: .slack, name: "trash-can",
            token: "bot-trash-can/BOT_TOKEN", appToken: "bot-trash-can/APP_TOKEN", allow: [],
            defaults: BotDefaults(projectId: api, provider: "claude", accountIds: ["claude"], model: "claude-opus-5-5",
                                  permissionMode: .fullAccess),
            channelBindings: [], instructions: nil)
        let page = BotPage(fleet: fleet, bots: MachineBots(hostId: "studio") { _ in .applied }, botId: "trash-can")
        try await snap(page.newThreadsSheet(bot, machine: studio), size: CGSize(width: 480, height: 520),
                       named: "elevation-new-threads.png")
    }

    private func snap(_ view: some View, size: CGSize, named name: String) async throws {
        let host = NSHostingView(rootView: view.environment(\.colorScheme, .dark))
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(origin: .zero, size: size)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        for _ in 0..<6 {
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
}
#endif
