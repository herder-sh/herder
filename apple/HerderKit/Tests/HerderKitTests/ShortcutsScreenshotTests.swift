#if os(macOS)
import AppKit
import Foundation
@testable import HerderKit
import SwiftUI
import Testing

/// Draws Settings → Shortcuts and the cheat sheet as the Mac shows them. With
/// `HERDER_SCREENSHOT_DIR` set, also writes them there. Drawn in a window, as the recorders are
/// AppKit's, which `ImageRenderer` leaves out.
@MainActor struct ShortcutsScreenshotTests {
    @Test func drawsTheShortcutsPane() async throws {
        try await draw(ShortcutsSettings(), size: CGSize(width: 520, height: 600), named: "settings-shortcuts")
    }

    @Test func drawsTheCheatSheet() async throws {
        let sheet = ZStack {
            Theme.canvas
            Color.black.opacity(0.35)
            CheatSheet {}
        }
        try await draw(sheet, size: CGSize(width: 900, height: 820), named: "cheat-sheet")
    }

    private func draw(_ content: some View, size: CGSize, named: String) async throws {
        let view = NSHostingView(rootView: content.environment(\.colorScheme, .dark).frame(width: size.width, height: size.height))
        view.frame = CGRect(origin: .zero, size: size)
        let window = NSWindow(contentRect: view.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: .darkAqua)
        window.contentView = view
        for _ in 0..<5 {
            view.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        let rep = try #require(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: rep)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: "\(named).png")
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(named).png"))
        }
    }
}
#endif
