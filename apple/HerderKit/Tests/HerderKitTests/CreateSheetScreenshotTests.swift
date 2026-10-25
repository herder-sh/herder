#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the Add Account sheet as mockup §11 has it: Claude picked, two machines ticked and a
/// member's machine disabled with its reason, and attaches it; with `HERDER_SCREENSHOT_DIR`
/// set, also writes it there as `add-account.png`. Drawn in a window, as its text fields are
/// AppKit's, which `ImageRenderer` leaves out.
@MainActor struct CreateSheetScreenshotTests {
    @Test func drawsAddAccountOnSeveralMachines() throws {
        let view = NSHostingView(rootView: AddAccountPreview().environment(\.colorScheme, .dark))
        view.frame = CGRect(x: 0, y: 0, width: 560, height: 540)
        let window = NSWindow(contentRect: view.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: .darkAqua)
        window.contentView = view
        view.layoutSubtreeIfNeeded()
        let rep = try #require(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: rep)
        #expect(rep.pixelsWide >= 560 && rep.pixelsHigh >= 540)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: "add-account.png")
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("add-account.png"))
        }
    }
}

private struct AddAccountPreview: View {
    @State private var draft = AccountDraft(provider: "claude", label: "work")
    @State private var ticked: Set<HostId> = ["work-mbp", "trash-can-01"]
    @State private var showsConfigDir = false

    var body: some View {
        SheetScaffold(title: "Add Account", height: 540, hint: "Logins run in Activity; this sheet closes.") {
            AddAccountForm(draft: $draft, ticked: $ticked, showsConfigDir: $showsConfigDir, targets: [
                AccountTarget(hostId: "work-mbp", name: "work-mbp", problem: nil),
                AccountTarget(hostId: "trash-can-01", name: "trash-can-01", problem: nil),
                AccountTarget(hostId: "draugelis", name: "draugelis", problem: "Member — can’t add accounts"),
            ], installing: { _ in false }, install: { _ in })
        } footer: {
            Spacer()
            ActionButton(title: AddAccountSheet.title(ticked.count), style: .primary) {}.fixedSize()
        }
    }
}
#endif
