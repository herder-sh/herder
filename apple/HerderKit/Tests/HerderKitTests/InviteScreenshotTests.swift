#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the People section as the app's dark appearance shows it, at the Mac's page width and
/// the iPhone's: for an owner, with the role menus, Revoke and Invite; for a member, the list
/// only; while asking and after a refusal; and the link an invite made. Attaches each; with
/// `HERDER_SCREENSHOT_DIR` set, also writes them there as `people-<state>-<width>.png`.
@MainActor struct PeopleScreenshotTests {
    private let now = Timestamp.date("2026-10-26T09:00:00Z")!

    @Test(arguments: [("mac", 920.0), ("iphone", 390.0)])
    func drawsEachState(width: (name: String, points: Double)) async throws {
        let users = People.people(PeopleTests.team)
        let states: [(String, Load<[UserInfo]>, Bool, String?)] = [
            ("owner", .loaded(users), true, nil),
            ("member", .loaded(users), false, nil),
            ("asking", .asking, true, nil),
            ("refused", .loaded(users), true, "the last owner cannot be made a member; make someone else an owner first"),
            ("failed", .failed("the machine did not answer"), false, nil),
        ]
        for (state, people, owner, error) in states {
            let list = PeopleList(people: people, owner: owner, error: error, now: now,
                                  setRole: { _, _ in }, revoke: { _ in }, invite: {})
            try await shoot(list, width: width.points, named: "people-\(state)-\(width.name).png")
        }
    }

    @Test(arguments: [("mac", 560.0), ("iphone", 390.0)])
    func drawsAnInvite(width: (name: String, points: Double)) async throws {
        let invite = People.Invite(
            name: "dana", role: .member,
            link: pairingUriToString(uri: PairingUri(hosts: ["devbox.tailnet.ts.net:7447", "192.168.1.20:7447"],
                                                     fingerprint: String(repeating: "9f86", count: 16),
                                                     code: "ABCDE-FGHJK")),
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
