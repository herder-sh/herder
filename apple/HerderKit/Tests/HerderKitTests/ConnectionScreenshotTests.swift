#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws how the apps say a machine is reached and where a sent message is (§5.9): a session
/// on a machine all is well with, where nothing extra shows; the same session with its machine
/// offline and a message waiting, at the Mac's width and an iPhone's; every delivery line under
/// a slow link; and the connection card, connected on the Mac and offline at an iPhone's width.
/// Attaches each; with `HERDER_SCREENSHOT_DIR` set, also writes them there.
@MainActor @Suite(.serialized) struct ConnectionScreenshotTests {
    static let key = SessionKey(hostId: "devbox", sessionId: "01K00000000000000000000001")

    @Test func drawsNothingExtraWhileAllIsWell() async throws {
        let fleet = try await sampleFleet(.connected, average: 38)
        try await shoot(SessionView(fleet: fleet, key: Self.key), size: CGSize(width: 900, height: 420),
                        named: "connection-header-mac.png")
    }

    @Test(arguments: [("mac", 900.0), ("iphone", 390.0)])
    func drawsAnOfflineMachineInTheHeaderAndOnAMessage(device: String, width: Double) async throws {
        let fleet = try await sampleFleet(.disconnected(error: "no answer in 10s"), outbox: [
            Outgoing(text: "Then run the e2e suite in Firefox too.", state: .sending, sentAt: .now.addingTimeInterval(-4)),
        ])
        try await shoot(SessionView(fleet: fleet, key: Self.key), size: CGSize(width: width, height: 640),
                        named: "connection-offline-\(device).png")
    }

    @Test func drawsEachDeliveryLine() async throws {
        let fleet = try await sampleFleet(.connected, average: 640, outbox: [
            Outgoing(text: "Delivered, waiting for the turn to end.", state: .delivered),
            Outgoing(text: "Sent a moment ago.", state: .sending, sentAt: .now.addingTimeInterval(-3)),
            Outgoing(text: "Sent a while ago, still not taken.", state: .sending, sentAt: .now.addingTimeInterval(-20)),
            Outgoing(text: "Refused by the machine.", state: .failed("the session is archived")),
        ])
        try await shoot(SessionView(fleet: fleet, key: Self.key), size: CGSize(width: 900, height: 720),
                        named: "connection-delivery-mac.png")
    }

    @Test func drawsTheCardConnected() async throws {
        let fleet = try await sampleFleet(.connected, average: 41, reconnects: 2, outbox: [
            Outgoing(text: "Next", state: .delivered),
        ])
        let card = ConnectionCard(fleet: fleet, hostId: "devbox", openMachine: {}).frame(width: 380)
        try await shoot(card, size: CGSize(width: 380, height: 300), named: "connection-card-mac.png")
    }

    @Test func drawsTheCardOfflineAtTheIPhonesWidth() async throws {
        let fleet = try await sampleFleet(.disconnected(error: "no answer in 10s"), outbox: [
            Outgoing(text: "Then run the e2e suite in Firefox too.", state: .sending, sentAt: .now.addingTimeInterval(-4)),
        ])
        let card = ConnectionCard(fleet: fleet, hostId: "devbox", openMachine: {}).frame(width: 390)
        try await shoot(card, size: CGSize(width: 390, height: 260), named: "connection-card-iphone.png")
    }

    /// devbox, reached over Tailscale while connected, with a session that asked one thing and
    /// was answered, and `outbox` on its way to it.
    private func sampleFleet(_ connection: ConnectionState, average: UInt32? = nil, reconnects: UInt32 = 0,
                             outbox: [Outgoing] = []) async throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        let api = "github.com/acme/api"
        var devbox = machine("devbox", name: "devbox", sessions: [Self.key.sessionId], projects: [
            Project(projectId: api, name: "api", paths: [], defaultPermissionMode: nil, defaultAccount: nil, setupCommand: nil),
        ], connection: connection)
        devbox.sessions[0].projectId = api
        devbox.addresses = ["devbox.tail1234.ts.net:7447"]
        if connection == .connected {
            devbox.address = "devbox.tail1234.ts.net:7447"
            devbox.quality.lastRttMs = average.map { $0 - 3 }
            devbox.quality.averageRttMs = average
            devbox.quality.minRttMs = average.map { $0 - 6 }
            devbox.quality.maxRttMs = average.map { $0 + 9 }
        }
        devbox.quality.reconnects = reconnects
        fleet.setMachinesForTesting([devbox])
        var script = Script(Self.key.sessionId, host: "devbox")
        let events = [
            script.event(created()),
            script.event(.titleChanged(title: "Run checkout e2e in more browsers", source: .auto)),
            script.event(.turnStarted(turnId: "t1")),
            script.event(.itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: "u1", turnId: "t1",
                                               body: .userMessage(text: "Run the checkout e2e suite in Safari.", attachments: [])))),
            script.event(.itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: "a1", turnId: "t1",
                                               body: .assistantMessage(text: "All 42 checkout tests pass in Safari.")))),
            script.event(.turnCompleted(turnId: "t1", usage: nil)),
        ]
        fleet.applyForTesting(SessionUpdate(events: events, streaming: []), to: script.key)
        fleet.setOutboxForTesting(outbox, of: Self.key)
        // The lists rebuild a moment after the sessions change.
        try await Task.sleep(for: .milliseconds(300))
        return fleet
    }

    private func shoot(_ content: some View, size: CGSize, named name: String) async throws {
        let view = content
            .frame(width: size.width, height: size.height, alignment: .top)
            .background(Theme.canvas)
            .environment(\.colorScheme, .dark)
        let host = NSHostingView(rootView: view)
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(origin: .zero, size: size)
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
}
#endif
