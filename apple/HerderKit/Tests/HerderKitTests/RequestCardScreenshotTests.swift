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

    /// An agent's `request_secret` as the card shows it with a long hint and the warning that it
    /// replaces a value, with a short hint, and squeezed to the height a pinned card has with the
    /// keyboard up. Drawn before the card asks the machine for its hub, which this test has none
    /// of; with `HERDER_SCREENSHOT_DIR` set, each is written there as `request-secret-<case>.png`.
    @Test(arguments: SecretCase.allCases)
    func drawsARequestSecretCard(_ shown: SecretCase) throws {
        let host = try hosted(secretCard(shown), height: shown.height)
        let png = try snapshot(host)
        Attachment.record(png, named: "request-secret-\(shown).png")
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("request-secret-\(shown).png"))
        }
    }

    /// Short of room, the hint gives way and the value field and Store stay in view: the one
    /// thing the card is for can never be scrolled out of sight.
    @Test func keepsTheValueFieldAndStoreInViewWhenShortOfRoom() throws {
        let host = try hosted(secretCard(.cramped), height: SecretCase.cramped.height)
        for id in ["request-secret-value", "request-secret-store"] {
            let frame = try #require(Self.frame(of: id, in: host), "the card has no \(id)")
            #expect(host.bounds.contains(frame), "\(id) at \(frame) is outside the card's \(host.bounds)")
        }
    }

    enum SecretCase: String, CaseIterable, CustomStringConvertible {
        case long, short, cramped
        var description: String { rawValue }

        /// The height the card gets; `nil` for as tall as it wants.
        var height: CGFloat? { self == .cramped ? 230 : nil }

        var secret: SecretRequest {
            switch self {
            case .short:
                SecretRequest(set: "homelab", name: "NPM_TOKEN", description: "npm automation token", replaces: false)
            case .long, .cramped:
                SecretRequest(
                    set: "homelab", name: "CF_HOMELAB_ACCESS_TOKEN",
                    description: "Cloudflare API token, Zone DNS edit on homelab.dev. "
                        + "https://dash.cloudflare.com/profile/api-tokens → Create Token → Edit zone DNS → homelab.dev",
                    replaces: true)
            }
        }
    }

    private func secretCard(_ shown: SecretCase) throws -> some View {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw ProfileUnavailable()
        }
        let session = SessionSummary(
            key: SessionKey(hostId: "h1", sessionId: "s1"), title: "Point homelab DNS at the new ingress",
            project: "homelab", branch: "", machine: "devbox", state: .needsYou, activity: "", age: "now")
        var request = PendingRequest(
            requestId: "secret-1", session: session,
            kind: .approval(summary: "Supply the secret \(shown.secret.name) in \(shown.secret.set)"),
            since: .now, age: "1m ago")
        request.secretRequest = shown.secret
        return RequestCard(request: request, fleet: fleet)
    }

    private struct ProfileUnavailable: Error {}

    /// `card` 460 points wide, as the app's dark appearance shows it, hosted in a window, as the
    /// field and Paste are AppKit's; `height` tall, else as tall as it fits.
    private func hosted(_ card: some View, height: CGFloat?) throws -> NSHostingView<AnyView> {
        let root = card
            .frame(width: 460)
            .frame(height: height, alignment: .bottom)
            .padding(20)
            .background(Theme.canvas)
            .environment(\.colorScheme, .dark)
        let host = NSHostingView(rootView: AnyView(root))
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(origin: .zero, size: host.fittingSize)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        host.layoutSubtreeIfNeeded()
        return host
    }

    private func snapshot(_ host: NSView) throws -> Data {
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        #expect(rep.pixelsWide >= 460)
        return try #require(rep.representation(using: .png, properties: [:]))
    }

    /// Where the element `id` shows in `host`, in its coordinates, as accessibility reports it.
    private static func frame(of id: String, in host: NSView) -> NSRect? {
        func find(_ element: Any) -> NSAccessibilityElementProtocol? {
            if let element = element as? NSAccessibilityElementProtocol & NSObject,
               element.accessibilityIdentifier() == id {
                return element
            }
            for child in (element as? NSObject)?.accessibilityChildren() ?? [] {
                if let found = find(child) { return found }
            }
            return nil
        }
        guard let element = find(host) as? NSObject, let window = host.window else { return nil }
        let onScreen = element.accessibilityFrame()
        let inWindow = window.convertFromScreen(onScreen)
        return host.convert(inWindow, from: nil)
    }
}
#endif
