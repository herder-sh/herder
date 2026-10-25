#if os(macOS)
import AppKit
import Foundation
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the Providers page as mockup §3 has it, as the app's dark appearance shows it, and
/// attaches it; with `HERDER_SCREENSHOT_DIR` set, also writes it there as `providers.png`.
@MainActor struct ProvidersScreenshotTests {
    @Test func drawsThePageWithItsProblemsAccountsAndVersions() throws {
        let renderer = ImageRenderer(content: ProvidersPreview().environment(\.colorScheme, .dark))
        renderer.scale = 2
        let image = try #require(renderer.nsImage)
        #expect(image.size.width == 900 && image.size.height >= 500)
        let tiff = try #require(image.tiffRepresentation)
        let png = try #require(NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]))
        Attachment.record(png, named: "providers.png")
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("providers.png"))
        }
    }
}

private struct ProvidersPreview: View {
    var body: some View {
        let overview = ProvidersFixture.overview()
        VStack(alignment: .leading, spacing: 0) {
            PageHeader(title: "Providers", status: overview.status) {
                PaneButton(title: "Update All (\(overview.updatable.count))", symbol: "arrow.down.circle") {}
                PaneButton(title: "Add Account", symbol: "plus", prominent: true) {}
            }
            ProvidersPage(overview: overview, report: ProvidersFixture.report, actions: ProvidersPage.Actions(
                open: { _ in }, signIn: { _ in }, retry: { _ in }, install: { _ in }, setUp: {}, addAccount: {}))
                .padding(.horizontal, 20)
                .padding(.vertical, 16)
        }
        .frame(width: 900)
        .background(Theme.background)
    }
}
#endif
