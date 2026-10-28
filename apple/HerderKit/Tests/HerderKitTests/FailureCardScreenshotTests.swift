#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws a signed-out session's auth card, under the "Needs sign-in" chip and a held message,
/// as the app's dark appearance shows it at the Mac's transcript width and an iPhone's, and
/// attaches both; with `HERDER_SCREENSHOT_DIR` set, also writes them there as
/// `auth-card-mac.png` and `auth-card-iphone.png`.
@MainActor struct FailureCardScreenshotTests {
    @Test(arguments: [("mac", 520.0), ("iphone", 390.0)])
    func drawsTheAuthCard(device: String, width: Double) throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("no profile")
            return
        }
        let failure = Failure(id: 7, turnId: "t2", error: TurnError(class: .auth, message: "Invalid API key · Please run /login"),
                              count: 2, prompt: "go", provider: "claude", accountId: "ops")
        let accounts = [
            Account(accountId: "ops", provider: "claude", label: "ops@", configDir: nil, email: nil, usage: [],
                    needsLogin: "2026-10-26T10:00:00Z"),
            Account(accountId: "work", provider: "claude", label: "work", configDir: nil, email: nil,
                    usage: [UsageWindow(window: "5h", usedPercent: 18, resetsAt: nil)]),
        ]
        let card = FailureCard(failure, FailureCard.Context(status: .needsLogin, held: 2, accounts: accounts,
                                                            machine: "trash-can-01", canSignIn: true))
        let view = VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 10) {
                Text("UI rethink").font(.title3.weight(.bold)).foregroundStyle(Theme.text)
                NeedsSignInChip()
            }
            HeldMessage(text: "go")
            FailureCardView(failure: failure, card: card, fleet: fleet, key: SessionKey(hostId: "h1", sessionId: "s1"))
        }
        .frame(width: width)
        .padding(16)
        .background(Theme.canvas)
        .environment(\.colorScheme, .dark)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 2
        let image = try #require(renderer.nsImage)
        #expect(image.size.width >= width)
        let tiff = try #require(image.tiffRepresentation)
        let png = try #require(NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]))
        let name = "auth-card-\(device).png"
        Attachment.record(png, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }
}
#endif
