#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws getting a second device onto your machines, at the Mac sheet's width and the
/// iPhone's: Add a Device with its QR code, signing in to the machines it shares with some
/// failing. With `HERDER_SCREENSHOT_DIR` set, also writes them there.
@MainActor struct AddDeviceScreenshotTests {
    nonisolated static let widths = [(560.0, "mac"), (390.0, "ios")]

    /// The names the sharing device gives the machines, as its link carries them.
    nonisolated static let names = [1: "devbox", 2: "nas", 3: "build-01", 4: "alice’s vault", 5: "mac-mini"]

    static func machine(_ n: Int, port: Int = 7447) -> FleetMachine {
        FleetMachine(hosts: ["100.64.0.\(n):\(port)"], fingerprint: String(repeating: String(format: "%02x", n), count: 32),
                     name: names[n])
    }

    @Test(arguments: widths)
    func drawsAddADevice(width: Double, platform: String) throws {
        let link = fleetLinkToString(link: FleetLink(machines: (1...5).map { Self.machine($0) }))
        try shoot(SheetPreview(title: "Add a Device", subtitle: AddDeviceSheet.subtitle, hint: "") {
            AddDeviceSteps(link: link)
        } footer: {
            ActionButton(title: "Done", style: .primary) {}.frame(maxWidth: 160)
        }, width: width, height: 1000, named: "\(platform)-add-device.png")
    }

    @Test(arguments: widths)
    func drawsSigningInToAnotherDevicesMachines(width: Double, platform: String) async throws {
        let join = FleetJoin(FleetLink(machines: (1...5).map { Self.machine($0) }),
                             known: { $0 == Self.machine(5).fingerprint ? "mac-mini" : nil }) { machine in
            switch machine.hosts[0] {
            case "100.64.0.2:7447": throw HerderError.Unreachable(detail: "timed out")
            case "100.64.0.4:7447": throw HerderError.Rejected(info: ErrorInfo(code: .forbidden, message: "unknown"))
            default: return machine.name ?? "?"
            }
        }
        await join.run()
        try shoot(SheetPreview(title: join.title, subtitle: JoinFleetView.subtitle, hint: join.summary(biometry: "Face ID")) {
            JoinFleetRows(join: join)
        } footer: {
            JoinFleetButtons(join: join, biometry: "Face ID")
        }, width: width, height: 900, named: "\(platform)-join-fleet.png")
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
                Spacer()
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
