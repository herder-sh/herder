#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws inviting a teammate and joining from the invite, as the app's dark appearance shows
/// them, at the Mac's sheet width and the iPhone's: the Invite form, the link it made, and the
/// teammate's first launch from it before and after joining. Attaches each; with
/// `HERDER_SCREENSHOT_DIR` set, also writes them there as `invite-<screen>-<platform>.png`.
@MainActor struct InviteScreenshotTests {
    nonisolated static let widths = [(560.0, "mac"), (390.0, "iphone")]
    private let now = Timestamp.date("2026-10-26T09:00:00Z")!
    private let sign: PasskeySigner = { _ in throw HerderError.Local(detail: "unused") }

    nonisolated static let names = ["devbox", "nas", "build-01", "studio", "mac-mini"]

    /// Each machine's invite, as it would mint it.
    nonisolated static func machine(_ index: Int) -> InviteLink.Machine {
        InviteLink.Machine(hosts: ["\(names[index]).tailnet.ts.net:7447", "192.168.1.\(20 + index):7447"],
                           fingerprint: String(repeating: String(format: "%02x", index + 0x90), count: 32),
                           invite: "Kq3vX9mPz2L\(index)", name: names[index])
    }

    private func maker() -> InviteMaker {
        let expiresAt = now.addingTimeInterval(9 * 60 + 41)
        let maker = InviteMaker(machines: Self.names.map { InviteMaker.Machine(hostId: $0, name: $0) }) { role, machine, _ in
            FleetPeople.Invite(role: role, machine: Self.machine(Self.names.firstIndex(of: machine.hostId) ?? 0),
                               expiresAt: expiresAt)
        }
        maker.name = "Lina"
        maker.by = "Tomas"
        return maker
    }

    @Test(arguments: widths)
    func drawsTheInviteForm(width: Double, platform: String) throws {
        let maker = maker()
        try shoot(InvitePreview(title: "Invite a Teammate",
                                subtitle: "They open the link in herder and join with a passkey of their own.",
                                hint: "Face ID asks once for each machine.") {
            InviteForm(maker: maker)
        } footer: {
            ActionButton(title: "Create Invite Link", style: .primary) {}.frame(maxWidth: 220)
        }, width: width, height: 760, named: "invite-form-\(platform).png")
    }

    @Test(arguments: widths)
    func drawsTheInviteLink(width: Double, platform: String) async throws {
        let maker = maker()
        await maker.make(sign: sign)
        let link = try #require(maker.link)
        try shoot(InvitePreview(title: "Invite for Lina", subtitle: link.summary, hint: "") {
            InviteLinkPanel(link: link, expiresAt: maker.expiresAt, now: now)
        } footer: {
            InviteLinkButtons(link: link.string)
        }, width: width, height: 620, named: "invite-link-\(platform).png")
    }

    private func join() -> InviteJoin {
        InviteJoin(InviteLink(machines: (0..<5).map(Self.machine), role: .member, by: "Tomas", label: "Lina"),
                   known: { _ in nil }) { machine, name in
            (machine.title, person("p-lina", name, .member))
        } accept: { machine, _ in
            switch machine.title {
            case "build-01": throw HerderError.Unreachable(detail: "timed out")
            case "studio": throw HerderError.Rejected(info: ErrorInfo(code: .forbidden, message: "used"))
            default: return machine.title
            }
        }
    }

    @Test(arguments: widths)
    func drawsJoiningFromAnInvite(width: Double, platform: String) throws {
        let join = join()
        try shoot(InvitePreview(title: "Join herder", subtitle: "", hint: join.hint(biometry: "Face ID")) {
            JoinInviteContent(join: join)
        } footer: {
            JoinInviteButtons(join: join, biometry: "Face ID")
        }, width: width, height: 860, named: "invite-join-\(platform).png")
    }

    @Test(arguments: widths)
    func drawsHowJoiningWent(width: Double, platform: String) async throws {
        let join = join()
        await join.run()
        try shoot(InvitePreview(title: join.title, subtitle: "", hint: join.hint(biometry: "Face ID")) {
            JoinInviteContent(join: join)
        } footer: {
            JoinInviteButtons(join: join, biometry: "Face ID")
        }, width: width, height: 620, named: "invite-joined-\(platform).png")
    }

    private func shoot(_ preview: some View, width: Double, height: Double, named name: String) throws {
        let view = NSHostingView(rootView: preview.environment(\.colorScheme, .dark).frame(width: width, height: height))
        view.frame = CGRect(x: 0, y: 0, width: width, height: height)
        let window = NSWindow(contentRect: view.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: .darkAqua)
        window.contentView = view
        view.layoutSubtreeIfNeeded()
        let rep = try #require(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: rep)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }
}

/// A sheet as the Mac draws it (`SheetScaffold`, 560 wide), or at the iPhone's width its
/// content as the phone's full-width sheet lays it out.
private struct InvitePreview<Content: View, Footer: View>: View {
    let title: String
    let subtitle: String
    let hint: String
    @ViewBuilder var content: Content
    @ViewBuilder var footer: Footer

    var body: some View {
        ViewThatFits(in: .horizontal) {
            SheetScaffold(title: title, subtitle: subtitle, height: 1000, hint: hint) {
                content
            } footer: {
                Spacer()
                footer
            }
            VStack(alignment: .leading, spacing: 0) {
                VStack(alignment: .leading, spacing: 3) {
                    Text(title).font(.title3.weight(.bold)).foregroundStyle(Theme.text)
                    if !subtitle.isEmpty {
                        Text(subtitle).font(.subheadline).foregroundStyle(Theme.secondary)
                    }
                }
                .padding(20)
                VStack(alignment: .leading, spacing: 18) { content }
                    .padding(.horizontal, 20)
                Spacer(minLength: 20)
                Rectangle().fill(Theme.stroke).frame(height: 1)
                VStack(alignment: .leading, spacing: 10) {
                    if !hint.isEmpty {
                        Text(hint).font(.footnote).foregroundStyle(Theme.tertiary)
                    }
                    HStack(spacing: 10) { footer }
                }
                .padding(16)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .layer(.sheet)
        }
    }
}
#endif
