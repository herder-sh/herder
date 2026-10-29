#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the link an invite made as the app's dark appearance shows it, at the Mac's sheet width
/// and the iPhone's. Attaches each; with `HERDER_SCREENSHOT_DIR` set, also writes them there as
/// `people-invite-<width>.png`.
@MainActor struct InviteScreenshotTests {
    private let now = Timestamp.date("2026-10-26T09:00:00Z")!

    @Test(arguments: [("mac", 560.0), ("iphone", 390.0)])
    func drawsAnInvite(width: (name: String, points: Double)) async throws {
        let invite = FleetPeople.Invite(
            role: .member,
            link: FleetPeople.inviteLink(addresses: ["devbox.tailnet.ts.net:7447", "192.168.1.20:7447"],
                                         fingerprint: String(repeating: "9f86", count: 16), invite: "Kq3vX9mPz2Lr"),
            expiresAt: now.addingTimeInterval(9 * 60 + 41))
        let view = VStack(alignment: .leading, spacing: 18) { InviteLinkView(invite: invite, now: now) }
        try await shoot(view, width: width.points, named: "people-invite-\(width.name).png")
    }

    private func shoot(_ content: some View, width: Double, named name: String) async throws {
        let view = content
            .padding(Theme.margin)
            .frame(width: width, alignment: .topLeading)
            .layer(.sheet)
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
