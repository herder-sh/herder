#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import ScreenCaptureKit
import SwiftUI
import Testing
import WebKit

/// Draws the artifact viewer as a window of its own, toolbar and page, on the demo's wide
/// mockup (1440 px of table) at Fit width: in a Mac-sized window and in an iPhone-sized one; and
/// its card in the thread. Attaches each, and with `HERDER_SCREENSHOT_DIR` set, also writes them
/// there as `artifact-mac.png`, `artifact-narrow.png` and `artifact-card.png`. A page renders out
/// of process, so these capture the window with ScreenCaptureKit, which needs the screen
/// recording permission and an unlocked screen; without them they are skipped.
@MainActor @Suite(.serialized) struct ArtifactScreenshotTests {
    @Test func showsTheWideMockupWholeAtFitWidth() async throws {
        let (fleet, key) = try sampleFleet()
        try await shoot(ArtifactViewer(fleet: fleet, key: key, start: 3), size: CGSize(width: 1280, height: 800),
                        named: "artifact-mac.png")
    }

    @Test func showsTheWideMockupWholeInANarrowWindow() async throws {
        let (fleet, key) = try sampleFleet()
        try await shoot(ArtifactViewer(fleet: fleet, key: key, start: 3), size: CGSize(width: 430, height: 800),
                        named: "artifact-narrow.png")
    }

    @Test func showsTheCardInTheThread() async throws {
        let (fleet, key) = try sampleFleet()
        let artifact = try #require(fleet.artifacts(of: key).last)
        let card = ArtifactBlock(artifact: artifact, fleet: fleet, key: key)
            .padding(24).frame(width: 720).background(Theme.background)
        try await shoot(card, size: CGSize(width: 720, height: 470), named: "artifact-card.png", titled: false)
    }

    @Test func showsTheCardsActionsAtMacWidth() async throws {
        try await snap(width: 720, named: "artifact-card-actions-mac.png")
    }

    @Test func foldsTheCardsActionsAtIPhoneWidth() async throws {
        try await snap(width: 390, named: "artifact-card-actions-iphone.png")
    }

    /// A published PNG's card, with its public link and then without, drawn without a screen
    /// capture: an image thumbnail has no page to paint out of process.
    private func snap(width: CGFloat, named name: String) async throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else { return }
        let icon = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().appendingPathComponent("App/Assets.xcassets/AppIcon.appiconset/icon-mac-512.png")
        let png = try Data(contentsOf: icon)
        var script = Script("01CARD", host: "studio")
        let shared = Attachment(attachmentId: "01ICON", mediaType: "image/png", size: UInt64(png.count),
                                name: "app-icon-1024.png")
        let kept = Attachment(attachmentId: "01KEPT", mediaType: "image/png", size: UInt64(png.count), name: "icon.png")
        let events = [
            script.event(created()),
            script.event(.artifactPublished(title: "trip1 app icon 1024x1024 for Meta", attachment: shared,
                                            url: "https://krowk.com/a/demo-icon", expiresAt: nil)),
            script.event(.artifactPublished(title: "trip1 app icon, private copy", attachment: kept, url: nil,
                                            expiresAt: nil)),
        ]
        fleet.applyForTesting(SessionUpdate(events: events, streaming: []), to: script.key)
        fleet.setAttachmentForTesting("01ICON", png)
        fleet.setAttachmentForTesting("01KEPT", png)
        let cards = VStack(spacing: 16) {
            ForEach(fleet.artifacts(of: script.key)) { ArtifactBlock(artifact: $0, fleet: fleet, key: script.key) }
        }
        .padding(16).frame(width: width).background(Theme.background).environment(\.colorScheme, .dark)
        let host = NSHostingView(rootView: cards)
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(x: 0, y: 0, width: width, height: 900)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        for _ in 0..<5 {
            host.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        host.frame.size.height = host.fittingSize.height
        host.layoutSubtreeIfNeeded()
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        let image = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(image, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try image.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }

    private func shoot(_ view: some View, size: CGSize, named name: String, titled: Bool = true) async throws {
        let host = NSHostingView(rootView: view.environment(\.colorScheme, .dark))
        host.sceneBridgingOptions = [.toolbars, .title]
        host.appearance = NSAppearance(named: .darkAqua)
        let window = NSWindow(contentRect: NSRect(origin: .zero, size: size),
                              styleMask: titled ? [.titled, .closable, .miniaturizable, .resizable]
                                  : [.titled, .fullSizeContentView],
                              backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: .darkAqua)
        if !titled {
            // ScreenCaptureKit captures titled windows only: this one hides its title bar.
            window.titlebarAppearsTransparent = true
            window.titleVisibility = .hidden
        }
        window.toolbarStyle = .unified
        window.contentView = host
        // WebKit paints only a window on a screen: this one goes behind every other.
        window.setFrameOrigin(NSScreen.main?.visibleFrame.origin ?? .zero)
        window.orderBack(nil)
        defer { window.orderOut(nil) }
        // The page loads, measures and fits.
        for _ in 0..<30 {
            host.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        guard let image = await capture(window) else { return }
        let png = try #require(try await withPages(of: window, on: image).representation(using: .png, properties: [:]))
        Attachment.record(png, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }

    /// The window as the screen shows it; `nil` where nothing can be captured: without the screen
    /// recording permission, or with the screen locked.
    private func capture(_ window: NSWindow) async -> CGImage? {
        guard let shareable = try? await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false),
              let target = shareable.windows.first(where: { $0.windowID == CGWindowID(window.windowNumber) })
        else { return nil }
        let config = SCStreamConfiguration()
        config.width = Int(window.frame.width * 2)
        config.height = Int(window.frame.height * 2)
        config.showsCursor = false
        return try? await SCScreenshotManager.captureImage(
            contentFilter: SCContentFilter(desktopIndependentWindow: target), configuration: config)
    }

    /// The capture with each page drawn in: WebKit paints no window hidden behind others, as
    /// this one is to keep off the screen, but snapshots its page all the same.
    private func withPages(of window: NSWindow, on image: CGImage) async throws -> NSBitmapImageRep {
        let scale = CGFloat(image.width) / window.frame.width
        let context = try #require(CGContext(data: nil, width: image.width, height: image.height, bitsPerComponent: 8,
                                             bytesPerRow: 0, space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                             bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
        for web in pages(in: try #require(window.contentView)) {
            let page = try await web.takeSnapshot(configuration: nil)
            let frame = web.convert(web.bounds, to: nil)
            let rect = CGRect(x: frame.minX * scale, y: frame.minY * scale, width: frame.width * scale,
                              height: frame.height * scale)
            if let cgPage = page.cgImage(forProposedRect: nil, context: nil, hints: nil) {
                context.draw(cgPage, in: rect)
            }
        }
        return NSBitmapImageRep(cgImage: try #require(context.makeImage()))
    }

    private func pages(in view: NSView) -> [WKWebView] {
        if let web = view as? WKWebView { return [web] }
        return view.subviews.flatMap(pages(in:))
    }

    /// A session that published a screenshot, then the demo's wide mockup.
    private func sampleFleet() throws -> (Fleet, SessionKey) {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        let page = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("crates/herder-ffi/fixtures/demo/invoices-mockup.html")
        let html = try Data(contentsOf: page)
        var script = Script("01ART", host: "studio")
        let shot = Attachment(attachmentId: "01SHOT", mediaType: "application/octet-stream", size: 1, name: "s.png")
        let mockup = Attachment(attachmentId: "01PAGE", mediaType: "application/octet-stream", size: UInt64(html.count),
                                name: "invoices-on-the-ledger-list-mockup.html")
        let events = [
            script.event(created()),
            script.event(.artifactPublished(title: "Settings screen", attachment: shot, url: nil, expiresAt: nil)),
            script.event(.artifactPublished(title: "Invoices on the ledger — list (mockup)", attachment: mockup,
                                            url: "https://krowk.com/a/demo-invoices", expiresAt: nil)),
        ]
        fleet.applyForTesting(SessionUpdate(events: events, streaming: []), to: script.key)
        fleet.setAttachmentForTesting("01PAGE", html)
        return (fleet, script.key)
    }
}
#endif
