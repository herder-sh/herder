#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws screens 2 and 4 of docs/mockups/identity.html on the mockup's demo fleet, as the app's
/// dark appearance shows them: approving a new machine and then its success, approving a
/// terminal's sign-in, and Claim your machines.
/// Attaches each; with `HERDER_SCREENSHOT_DIR` set, also writes them there as
/// `identity-<screen>-<width>.png`.
@MainActor struct IdentityScreenshotTests {
    private let now = Timestamp.date("2026-10-29T12:00:00Z")!

    @Test func drawsApprovingAMachine() async throws {
        let approval = Approval(ApprovalTests.machine, authenticator: FakeAuthenticator(), challenge: { ApprovalTests.challenge }) { _ in }
        let sheet = { (biometry: String) in
            ApprovalSheet(approval: approval, teammates: ["Sam", "Kim"], vaults: ["herder-vault-0", "draugelis vault"],
                          biometry: biometry)
        }
        try await shoot(phone(sheet("Face ID")), width: Self.phone.width, layer: .canvas, named: "identity-approve-iphone.png")
        try await shoot(sheet("Touch ID"), width: Self.mac, named: "identity-approve-mac.png")
        await approval.approve()
        try await shoot(phone(sheet("Face ID")), width: Self.phone.width, layer: .canvas, named: "identity-approved-iphone.png")
        try await shoot(sheet("Touch ID"), width: Self.mac, named: "identity-approved-mac.png")
    }

    @Test func drawsApprovingATerminal() async throws {
        let request = ApprovalRequest(kind: .login, name: "trash-can-01", code: "K7Q4PD",
                                      fingerprint: ApprovalTests.fingerprint, hosts: ["10.0.4.21:7447"], version: "0.17.0")
        let approval = Approval(request, authenticator: FakeAuthenticator(), challenge: { ApprovalTests.challenge }) { _ in }
        try await shoot(phone(ApprovalSheet(approval: approval, biometry: "Face ID")), width: Self.phone.width, layer: .canvas,
                        named: "identity-login-iphone.png")
        try await shoot(ApprovalSheet(approval: approval, biometry: "Touch ID"), width: Self.mac, named: "identity-login-mac.png")
        await approval.approve()
        try await shoot(phone(ApprovalSheet(approval: approval, biometry: "Face ID")), width: Self.phone.width, layer: .canvas,
                        named: "identity-login-done-iphone.png")
    }

    @Test(arguments: [("mac", 640.0), ("iphone", 390.0)])
    func drawsClaim(width: (name: String, points: Double)) async throws {
        let claim = Claim(.demo, authenticator: FakeAuthenticator()) { _, _, _ in }
        let sheet = ClaimSheet(claim: claim, name: "Tomas", userId: Data(), challenge: Data())
            // The iPhone's sheet fills the screen; the Mac's is its own height.
            .frame(width: width.points, height: width.name == "iphone" ? Self.phone.height : nil)
        try await shoot(sheet, width: width.points, named: "identity-claim-\(width.name).png")
    }

    /// The Mac's approval sheet.
    private static let mac = 400.0

    /// An iPhone's screen: Fleet behind, the sheet risen from the bottom.
    private static let phone = CGSize(width: 390, height: 844)

    private func phone(_ sheet: some View) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Fleet").font(Theme.body).foregroundStyle(Theme.secondary).padding(.horizontal, 18).padding(.top, 56)
            Spacer(minLength: 0)
            sheet.clipShape(.rect(cornerRadius: 22)).padding(.horizontal, 8).padding(.bottom, 8)
        }
        .frame(width: Self.phone.width, height: Self.phone.height)
    }

    private func shoot(_ content: some View, width: Double, layer: Layer = .sheet, named name: String) async throws {
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
