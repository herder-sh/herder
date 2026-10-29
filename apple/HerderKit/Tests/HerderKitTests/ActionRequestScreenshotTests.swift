#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the sheet a TUI's terminal request brings up, in the P17.5 approve-sheet style: asking,
/// waiting on Face ID, and after Face ID was closed. Attaches each; with `HERDER_SCREENSHOT_DIR`
/// set, also writes them there as `action-request-<state>-<width>.png`.
@MainActor struct ActionRequestScreenshotTests {
    private let now = Timestamp.date("2026-10-29T12:00:08Z")!

    @Test func drawsATerminalsRequest() async throws {
        let request = ActionRequest(hostId: "h", machine: "trash-can-01", request: ActionRequestTests.request("r"))
        // What closed the prompt, once one was.
        var closed: String?
        let approval = ActionApproval(request) { _ in
            if let closed { throw HerderError.Local(detail: "\(closed) was cancelled") }
            try await Task.sleep(for: .seconds(60))
        }
        let sheet = { (biometry: String) in ActionRequestSheet(approval: approval, biometry: biometry, now: now) }
        try await shoot(phone(sheet("Face ID")), width: Self.phone.width, layer: .canvas, named: "action-request-asking-iphone.png")
        try await shoot(sheet("Touch ID"), width: Self.mac, named: "action-request-asking-mac.png")
        let signing = Task { await approval.approve() }
        while approval.state != .signing { await Task.yield() }
        try await shoot(phone(sheet("Face ID")), width: Self.phone.width, layer: .canvas, named: "action-request-signing-iphone.png")
        try await shoot(sheet("Touch ID"), width: Self.mac, named: "action-request-signing-mac.png")
        signing.cancel()
        await signing.value
        closed = "Face ID"
        await approval.approve()
        try await shoot(phone(sheet("Face ID")), width: Self.phone.width, layer: .canvas, named: "action-request-cancelled-iphone.png")
        closed = "Touch ID"
        await approval.approve()
        try await shoot(sheet("Touch ID"), width: Self.mac, named: "action-request-cancelled-mac.png")
    }

    /// The Mac's sheet.
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
