#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the People page of docs/mockups/identity.html on the mockup's demo fleet, as the app's
/// dark appearance shows it, at the Mac's width and the iPhone's. Attaches each; with
/// `HERDER_SCREENSHOT_DIR` set, also writes them there as `identity-people-<width>.png`.
@MainActor struct PeoplePageScreenshotTests {
    private let now = Timestamp.date("2026-10-29T12:00:00Z")!

    @Test(arguments: [("mac", 720.0), ("iphone", 390.0)])
    func drawsPeople(width: (name: String, points: Double)) async throws {
        let directory = Directory.demo(now: now)
        let page = VStack(alignment: .leading, spacing: 0) {
            PageHeader(title: "People", status: directory.status) {
                PaneButton(title: "Invite", symbol: "plus") {}
            }
            PeopleView(directory: directory, grid: width.name == "mac", now: now,
                       setRole: { _, _, _ in }, revoke: { _ in }, invite: {})
                .padding(.horizontal, Theme.margin)
                .padding(.vertical, 8)
        }
        try await shoot(page, width: width.points, layer: .canvas, named: "identity-people-\(width.name).png")
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
