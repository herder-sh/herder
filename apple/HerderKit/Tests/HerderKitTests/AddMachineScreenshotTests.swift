#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws Add to Fleet at the Mac sheet's width and the iPhone's: the link `herder claim` printed
/// on a machine, and a vault chosen from the Fleet page, to sign in to with your passkey. With
/// `HERDER_SCREENSHOT_DIR` set, also writes them there. Drawn in a window, as its text fields are
/// AppKit's, which `ImageRenderer` leaves out.
@MainActor struct AddMachineScreenshotTests {
    static let fingerprint = "3f9a1c0be47d2a865f01c3d9e8b7a6541203fedcba9876543210abcdef012345"
    static let vault = UnpairedVault(address: "127.0.0.1:7448", fingerprint: fingerprint,
                                     hosts: ["draugelis"], machine: "draugelis")

    @Test(arguments: [(560.0, "mac"), (390.0, "ios")])
    func drawsAMachineLinkThatMakesYouOwner(width: Double, platform: String) throws {
        let link = "herder://own?host=192.168.1.20%3A7447&fp=\(Self.fingerprint)&code=K7Q-4PD&name=work-mbp"
        try shoot(AddMachinePreview(kind: .machine, vault: nil, text: link, height: 820), width: width, height: 820,
                  named: "\(platform)-add-machine.png")
    }

    @Test(arguments: [(560.0, "mac"), (390.0, "ios")])
    func drawsAVaultChosenToSignInTo(width: Double, platform: String) throws {
        try shoot(AddMachinePreview(kind: .vault, vault: Self.vault, text: "", height: 900), width: width, height: 900,
                  named: "\(platform)-add-vault.png")
    }

    private func shoot(_ preview: AddMachinePreview, width: Double, height: Double, named name: String) throws {
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

private struct AddMachinePreview: View {
    @State var kind: PairKind
    @State var vault: UnpairedVault?
    @State var text: String
    let height: Double

    var body: some View {
        // The Mac's scaffold is 560 wide; at the iPhone's width, the form as its sheet lays it out.
        ViewThatFits(in: .horizontal) {
            SheetScaffold(title: "Add to Fleet", height: height, hint: "Next, check the code matches the terminal’s.") {
                form
            } footer: {
                Spacer()
                ActionButton(title: "Continue", style: .primary) {}.frame(width: 160)
            }
            VStack(alignment: .leading, spacing: 18) {
                Text("Add to Fleet").font(.title3.weight(.bold)).foregroundStyle(Theme.text)
                form
                Spacer(minLength: 0)
            }
            .padding(20)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .layer(.sheet)
        }
    }

    private var form: some View {
        AddMachineForm(kind: $kind, vault: $vault, text: $text, vaults: [AddMachineScreenshotTests.vault])
    }
}
#endif
