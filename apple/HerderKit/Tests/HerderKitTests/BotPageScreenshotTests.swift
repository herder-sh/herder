#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the `trash-can` bot's page as the app's dark appearance shows it, beside the 380 pt list
/// pane's width and at the window's full width, and Kim's sheet, and attaches each; with
/// `HERDER_SCREENSHOT_DIR` set, also writes them there as `bot-page.png`, `bot-page-wide.png` and
/// `bot-person.png`.
@MainActor struct BotPageScreenshotTests {
    @Test func drawsTheBotPage() async throws {
        try await shoot(width: 1040, named: "bot-page.png")
    }

    @Test func drawsTheBotPageAtFullWidth() async throws {
        try await shoot(width: 1440, named: "bot-page-wide.png")
    }

    @Test func drawsAPersonsSheet() async throws {
        let kim = BotUser(externalUserId: "U04KIM0003", displayName: "Kim", dm: true, channels: true, approve: false,
                          notify: false)
        try await snap(BotPersonSheet(bot: "trash-can", user: kim, set: { _ in }, remove: {}),
                       size: CGSize(width: 480, height: 470), named: "bot-person.png")
    }

    private func shoot(width: CGFloat, named name: String) async throws {
        let (fleet, bots) = try sample()
        let view = VStack(spacing: 0) {
            PageTrail(crumbs: ["Integrations", "trash-can"]) { _ in }
            BotPage(fleet: fleet, bots: bots, botId: "trash-can")
        }
        .background(Theme.canvas)
        try await snap(view, size: CGSize(width: width, height: 1000), named: name)
    }

    private func snap(_ view: some View, size: CGSize, named name: String) async throws {
        let host = NSHostingView(rootView: view.environment(\.colorScheme, .dark))
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(origin: .zero, size: size)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        // The page asks for its bots, then shows its index once it has read its sections.
        for _ in 0..<8 {
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

    /// `studio` with the projects `api` and `web` and a Claude account, running `trash-can`:
    /// Sam approves, Kim drives, Alex only DMs it, and `#web` works on `web`.
    private func sample() throws -> (Fleet, MachineBots) {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        let api = "github.com/acme/api"
        let web = "github.com/acme/web"
        let studio = machine("studio", name: "studio", sessions: [], projects: [
            Project(projectId: api, name: "api", paths: [], defaultPermissionMode: nil, defaultAccount: nil, setupCommand: nil),
            Project(projectId: web, name: "web", paths: [], defaultPermissionMode: nil, defaultAccount: nil, setupCommand: nil),
        ], accounts: [
            Account(accountId: "claude", provider: "claude", label: "claude", configDir: nil, email: "sam@acme.dev",
                    usage: [], fallback: false, needsLogin: nil),
        ])
        fleet.setMachinesForTesting([studio])
        let bot = Bot(
            botId: "trash-can", kind: .slack, name: "trash-can",
            token: "bot-trash-can/BOT_TOKEN", appToken: "bot-trash-can/APP_TOKEN",
            allow: [
                BotUser(externalUserId: "U04SAM0001", displayName: "Sam", dm: true, channels: true, approve: true, notify: false),
                BotUser(externalUserId: "U04KIM0003", displayName: "Kim", dm: true, channels: true, approve: false, notify: false),
                BotUser(externalUserId: "U04ALEX002", displayName: "Alex", dm: true, channels: false, approve: false, notify: false),
            ],
            defaults: BotDefaults(projectId: api, provider: "claude", accountIds: ["claude"], model: "claude-opus-5-5",
                                  permissionMode: .fullAccess),
            channelBindings: [ChannelBinding(channelId: "C04WEB0001", projectId: web)], instructions: nil)
        let bots = MachineBots(hostId: "studio") { command in
            switch command {
            case .listBots: .bots(bots: [bot])
            case .getBotStatus: .botStatus(bots: [BotStatus(botId: bot.botId, state: .connected, lastError: nil)])
            case .getBotMemory: .botMemory(botId: bot.botId, memory: BotMemory(facts: []))
            default: .applied
            }
        }
        return (fleet, bots)
    }
}
#endif
