#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws getting a second device onto your machines, at the Mac sheet's width and the
/// iPhone's: Add a device with its QR code and the machines it shares; the receiving side with
/// each machine reached before Face ID, after signing in with some needing you, and done.
/// With `HERDER_SCREENSHOT_DIR` set, also writes them there.
@MainActor struct AddDeviceScreenshotTests {
    nonisolated static let widths = [(560.0, "mac"), (390.0, "ios")]

    /// The machines the sharing device names in its link: mini-k8s only on Tailscale.
    nonisolated static let machines = [
        ("devbox", "192.168.1.20"), ("nas", "192.168.1.31"), ("build-01", "192.168.1.147"),
        ("mini-k8s", "100.71.3.22"), ("studio", "192.168.1.60"),
    ]

    static func machine(_ n: Int) -> FleetMachine {
        FleetMachine(hosts: ["\(machines[n].1):7447"], fingerprint: String(repeating: String(format: "%02x", n + 1), count: 32),
                     name: machines[n].0)
    }

    static let link = FleetLink(machines: machines.indices.map(machine))

    static func device(_ platform: String) -> (device: String, biometry: String) {
        platform == "mac" ? ("Mac", "Touch ID") : ("iPhone", "Face ID")
    }

    /// The receiving side of `link`: mini-k8s unreachable, studio knowing you by another
    /// passkey, nas knowing nobody by this one.
    static func join(passkey: FleetJoin.Passkey = .init(), refuses: Bool = true) -> FleetJoin {
        FleetJoin(link, known: { _ in nil }, passkey: passkey, check: { machine in
            if machine.name == "mini-k8s" { throw HerderError.Unreachable(detail: "timed out") }
        }) { machine in
            if refuses, machine.name == "studio" {
                throw HerderError.SignInRefused(refusal: .otherPasskey(passkeys: [
                    RefusedPasskey(label: "Tomas", aaguid: "bada5566-a7aa-401f-bd96-45619a55120d"),
                ]), detail: "another passkey")
            }
            if refuses, machine.name == "nas" {
                throw HerderError.SignInRefused(refusal: .unknownPerson(owners: ["Ada"]), detail: "unknown")
            }
            return machine.name ?? "?"
        }
    }

    @Test(arguments: widths)
    func drawsAddADevice(width: Double, platform: String) throws {
        try shoot(SheetPreview(title: AddDeviceSheet.title, subtitle: AddDeviceSheet.subtitle, hint: AddDeviceSheet.hint) {
            AddDeviceContent(link: Self.link)
        } footer: {
            AddDeviceButtons(link: Self.link)
        }, width: width, height: 860, named: "\(platform)-add-device.png")
    }

    /// Before Face ID: every machine reached but mini-k8s, which says why; the passkey as the
    /// link does not name it yet.
    @Test(arguments: widths)
    func drawsReachingTheMachinesBeforeSigningIn(width: Double, platform: String) async throws {
        let join = Self.join()
        await join.check()
        try shoot(join: join, platform: platform, width: width, named: "\(platform)-join-fleet.png")
    }

    /// After Face ID: two signed in, studio and nas refused the passkey, mini-k8s still unreachable;
    /// the passkey named, as once the link carries it.
    @Test(arguments: widths)
    func drawsWhatIsLeftAfterSigningIn(width: Double, platform: String) async throws {
        let join = Self.join(passkey: FleetJoin.Passkey(name: "Tomas", store: "Apple Passwords"))
        await join.check()
        await join.run()
        try shoot(join: join, platform: platform, width: width, named: "\(platform)-join-fleet-left.png")
    }

    @Test(arguments: widths)
    func drawsSignedInToEveryMachine(width: Double, platform: String) async throws {
        let join = FleetJoin(Self.link, known: { _ in nil }, check: { _ in }) { $0.name ?? "?" }
        await join.check()
        await join.run()
        try shoot(join: join, platform: platform, width: width, named: "\(platform)-join-fleet-done.png")
    }

    private func shoot(join: FleetJoin, platform: String, width: Double, named name: String) throws {
        let (device, biometry) = Self.device(platform)
        try shoot(SheetPreview(title: join.title(device: device), subtitle: join.subtitle, hint: join.hint(biometry: biometry)) {
            JoinFleetContent(join: join, device: device, biometry: biometry)
        } footer: {
            JoinFleetButtons(join: join, biometry: biometry)
        }, width: width, height: 1000, named: name)
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
private struct SheetPreview<Content: View, Footer: View>: View {
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
                footer
            }
            VStack(alignment: .leading, spacing: 0) {
                VStack(alignment: .leading, spacing: 3) {
                    Text(title).font(.title3.weight(.bold)).foregroundStyle(Theme.text)
                    Text(subtitle).font(.subheadline).foregroundStyle(Theme.secondary)
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
