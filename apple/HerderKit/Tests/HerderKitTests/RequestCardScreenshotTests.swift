import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing
#if os(macOS)
import AppKit

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
    /// keyboard up; as wide as on the Mac and on an iPhone. Drawn before the card asks the machine
    /// for its hub, which this test has none of; with `HERDER_SCREENSHOT_DIR` set, each is written
    /// there as `request-secret-<case>-<mac|iphone>.png`.
    @Test(arguments: RequestCardScreenshotCase.allCases, [460.0, 358.0])
    func drawsARequestSecretCard(_ shown: RequestCardScreenshotCase, width: Double) throws {
        let host = try hosted(secretCard(shown), width: width, height: shown.height)
        let png = try snapshot(host)
        let name = "request-secret-\(shown)-\(width < 400 ? "iphone" : "mac").png"
        Attachment.record(png, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }

    /// Short of room, the hint gives way and the value field and Paste, which turns into Store
    /// once there is a value, stay in full view: the one thing the card is for can never be
    /// scrolled out of sight.
    @Test func keepsTheValueFieldAndPasteInViewWhenShortOfRoom() throws {
        let host = try hosted(secretCard(.cramped), width: 358, height: RequestCardScreenshotCase.cramped.height)
        let views = Self.subviews(of: host)
        // Paste, drawn by the system with no view of its own, shares the field's row.
        let field = try #require(views.first { $0 is NSSecureTextField }, "the card has no value field")
        #expect(Self.inFullView(field, in: host), "the value field is cut off or scrolled out of view")
    }

    /// Whether all of `view` shows: inside `host` and every scroll view it is in.
    private static func inFullView(_ view: NSView, in host: NSView) -> Bool {
        var ancestor = view.superview
        while let clip = ancestor, clip !== host {
            if clip is NSClipView, !clip.bounds.contains(clip.convert(view.bounds, from: view)) { return false }
            ancestor = clip.superview
        }
        return host.bounds.contains(host.convert(view.bounds, from: view))
    }

    private static func subviews(of view: NSView) -> [NSView] {
        view.subviews + view.subviews.flatMap(subviews(of:))
    }

    private func secretCard(_ shown: RequestCardScreenshotCase) throws -> some View {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw ProfileUnavailable()
        }
        return RequestCard(request: shown.request, fleet: fleet)
    }

    private struct ProfileUnavailable: Error {}

    /// `card` `width` points wide, as the app's dark appearance shows it, hosted in a window, as
    /// the field and Paste are AppKit's; `height` tall, else as tall as it fits.
    private func hosted(_ card: some View, width: Double, height: CGFloat?) throws -> NSHostingView<AnyView> {
        let root = card
            .frame(width: width)
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
        return try #require(rep.representation(using: .png, properties: [:]))
    }

}
#endif

/// The `request_secret` cards the screenshot tests draw: a long hint with the warning that it
/// replaces a value, a short hint, and the long one squeezed to the height a pinned card has with
/// the keyboard up.
enum RequestCardScreenshotCase: String, CaseIterable, CustomStringConvertible {
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

    var request: PendingRequest {
        let session = SessionSummary(
            key: SessionKey(hostId: "h1", sessionId: "s1"), title: "Point homelab DNS at the new ingress",
            project: "homelab", branch: "", machine: "devbox", state: .needsYou, activity: "", age: "now")
        var request = PendingRequest(
            requestId: "secret-1", session: session,
            kind: .approval(summary: "Supply the secret \(secret.name) in \(secret.set)"),
            since: .now, age: "1m ago")
        request.secretRequest = secret
        return request
    }
}
