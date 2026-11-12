#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the You page of design/onboarding (flow 4) on the mockup's fleet, at the Mac's width
/// and the iPhone's: the page, a passkey's sheet, Add a passkey before and while it adds, and
/// Recovery, covered and not. Attaches each; with `HERDER_SCREENSHOT_DIR` set, also writes them
/// there as `you-*.png`.
@MainActor struct YouScreenshotTests {
    private let now = Timestamp.date("2026-11-12T12:00:00Z")!
    nonisolated static let widths = [("mac", 720.0), ("iphone", 390.0)]

    @Test(arguments: widths)
    func drawsYou(width: (name: String, points: Double)) async throws {
        let you = You.demo(now: now)
        let page = VStack(alignment: .leading, spacing: 22) {
            YouHeader(you: you)
            YouSections(you: you, open: { _ in }, addPasskey: {}, signOut: { _ in }, addDevice: {}, recovery: {})
        }
        .padding(.horizontal, Theme.margin)
        .padding(.bottom, 24)
        try await shoot(page, width: width.points, named: "you-\(width.name).png")
    }

    @Test(arguments: widths)
    func drawsRecovery(width: (name: String, points: Double)) async throws {
        var alone = You.demo(now: now)
        alone.passkeys.removeLast()
        let page = VStack(alignment: .leading, spacing: 32) {
            RecoveryView(you: You.demo(now: now)) {}
            RecoveryView(you: alone) {}
        }
        .padding(.horizontal, Theme.margin)
        .padding(.bottom, 24)
        try await shoot(page, width: width.points, named: "you-recovery-\(width.name).png")
    }

    @Test func drawsAPasskeysSheet() async throws {
        let you = You.demo(now: now)
        try await shoot(PasskeyDetails(you: you, passkey: you.passkeys[1]) {}.frame(height: 560),
                        width: 480, layer: .sheet, named: "you-passkey-sheet.png")
    }

    @Test func drawsAddingAPasskey() async throws {
        let you = You.demo(now: now)
        let picking = PasskeyAdding(personId: you.person.id, machines: you.columns, send: { _, _ in .applied },
                                    sign: Fake.sign, create: { _, _ in Fake.registration },
                                    signNew: { challenge, id, _ in Fake.assertion(id, over: challenge.challenge) })
        picking.place = .securityKey
        try await shoot(AddPasskeyView(adding: picking, biometry: "Touch ID"), width: 560, layer: .sheet,
                        named: "you-add-passkey.png")

        // mini-k8s takes its time, so the sheet shows three added, one adding and one next.
        let adding = PasskeyAdding(
            personId: you.person.id, machines: you.columns,
            send: { hostId, command in
                switch command {
                case .passkeyChallenge:
                    if hostId == "mini-k8s" { try await Task.sleep(for: .seconds(30)) }
                    return Fake.challenge(hostId)
                case .listPeople:
                    return .people(people: [PersonInfo(personId: you.person.id, name: "Tomas", role: .owner,
                                                       passkeys: [Fake.newKey], devices: [])])
                default: return .applied
                }
            },
            sign: Fake.sign, create: { _, _ in Fake.registration },
            signNew: { challenge, id, _ in Fake.assertion(id, over: challenge.challenge) })
        let running = Task { await adding.run() }
        for _ in 0..<200 where adding.rows[3].state != .adding { try await Task.sleep(for: .milliseconds(10)) }
        #expect(adding.rows.map(\.state) == [.added, .added, .added, .adding, .waiting])
        try await shoot(AddPasskeyView(adding: adding, biometry: "Touch ID"), width: 560, layer: .sheet,
                        named: "you-adding-passkey.png")
        running.cancel()
        await running.value
    }

    private func shoot(_ content: some View, width: Double, layer: Layer = .canvas, named name: String) async throws {
        let view = content
            .frame(width: width, alignment: .topLeading)
            .fixedSize(horizontal: false, vertical: true)
            .layer(layer)
            .environment(\.colorScheme, .dark)
        let host = NSHostingView(rootView: view)
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(origin: .zero, size: host.fittingSize)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        for _ in 0..<3 {
            host.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(50))
        }
        host.frame = NSRect(origin: .zero, size: host.fittingSize)
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
