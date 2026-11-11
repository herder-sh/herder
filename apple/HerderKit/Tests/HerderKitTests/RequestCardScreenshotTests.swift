#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws a multi-select question's card, a box per choice and Submit, as the app's dark
/// appearance shows it, and attaches it; with `HERDER_SCREENSHOT_DIR` set, also writes it there
/// as `multi-select-card.png`.
@MainActor struct RequestCardScreenshotTests {
    @Test func drawsAMultiSelectCard() throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("no profile")
            return
        }
        let session = SessionSummary(
            key: SessionKey(hostId: "h1", sessionId: "s1"), title: "Run checkout e2e in more browsers",
            project: "web", branch: "", machine: "laptop", state: .needsYou, activity: "", age: "now")
        let text = """
            Which browsers should the checkout suite also run in?

            - **WebKit (Recommended)**: Safari's engine, where most checkout bugs were reported
            - **Firefox**: Desktop Firefox
            - **Mobile Safari**: iPhone viewport on WebKit
            """
        let request = PendingRequest(
            requestId: "q1", session: session,
            kind: .question(text: text, choices: ["WebKit (Recommended)", "Firefox", "Mobile Safari"], multiSelect: true),
            since: .now, age: "2m ago")
        let card = RequestCard(request: request, fleet: fleet, answersInComposer: true)
            .frame(width: 460)
            .padding(20)
            .background(Theme.canvas)
            .environment(\.colorScheme, .dark)
        let renderer = ImageRenderer(content: card)
        renderer.scale = 2
        let image = try #require(renderer.nsImage)
        #expect(image.size.width >= 460)
        let tiff = try #require(image.tiffRepresentation)
        let png = try #require(NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]))
        Attachment.record(png, named: "multi-select-card.png")
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("multi-select-card.png"))
        }
    }

    /// An agent's `request_secret`: the name, the agent's hint, the warning that it replaces a
    /// value, the set, the secure field with Paste, and Decline and Store. Hosted in a window,
    /// as the field, the picker and Paste are AppKit's; drawn before the card asks the machine
    /// for its vault, which this test has none of.
    @Test func drawsARequestSecretCard() throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("no profile")
            return
        }
        let session = SessionSummary(
            key: SessionKey(hostId: "h1", sessionId: "s1"), title: "Ship the SIEM audit-log export",
            project: "siem", branch: "", machine: "devbox", state: .needsYou, activity: "", age: "now")
        var request = PendingRequest(
            requestId: "secret-1", session: session,
            kind: .approval(summary: "Supply the secret GITHUB_SIEM_PAT in coingate, replacing its value"),
            since: .now, age: "1m ago")
        request.secretRequest = SecretRequest(
            set: "coingate", name: "GITHUB_SIEM_PAT",
            description: "GitHub fine-grained PAT, read-only on the org audit log", replaces: true)
        let card = RequestCard(request: request, fleet: fleet)
            .frame(width: 460)
            .padding(20)
            .background(Theme.canvas)
            .environment(\.colorScheme, .dark)
        let host = NSHostingView(rootView: card)
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(origin: .zero, size: host.fittingSize)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        host.layoutSubtreeIfNeeded()
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        #expect(rep.pixelsWide >= 460)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: "request-secret-card.png")
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("request-secret-card.png"))
        }
    }
}
#endif
