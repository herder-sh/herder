#if os(macOS)
import AppKit
import Foundation
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the Providers page as mockup §3 has it, as the app's dark appearance shows it, full
/// width and as narrow as the Mac's list pane, and attaches each; with `HERDER_SCREENSHOT_DIR`
/// set, also writes them there as `providers.png` and `providers-380.png`.
@MainActor struct ProvidersScreenshotTests {
    @Test(arguments: [(900, "providers.png"), (380, "providers-380.png")])
    func drawsThePageWithItsProblemsAccountsAndVersions(width: Int, name: String) throws {
        let renderer = ImageRenderer(content: ProvidersPreview(width: CGFloat(width)).environment(\.colorScheme, .dark))
        renderer.scale = 2
        let image = try #require(renderer.nsImage)
        #expect(image.size.width == CGFloat(width) && image.size.height >= 500)
        let tiff = try #require(image.tiffRepresentation)
        let png = try #require(NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]))
        Attachment.record(png, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }
}

private struct ProvidersPreview: View {
    let width: CGFloat

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
        .frame(width: width)
        .background(Theme.background)
    }
}
#endif
