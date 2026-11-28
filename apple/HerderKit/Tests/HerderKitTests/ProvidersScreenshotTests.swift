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

/// Draws the Providers page over the fleet P26.26 tidies (`ProvidersFixture.tidyMachines`): each
/// provider's hub logins apart from its machines' own, one row per email with its machines, the
/// hub's proxy by name, a hub login's typo label beside its email, blank limits and the one-line
/// set-up hint; at the Mac's width and an iPhone's. With `HERDER_SCREENSHOT_DIR` set, also writes
/// them there as `providers-tidy-mac.png` and `providers-tidy-iphone.png`.
@MainActor struct ProvidersTidyScreenshotTests {
    @Test(arguments: [(900, "providers-tidy-mac.png"), (393, "providers-tidy-iphone.png")])
    func drawsTheGroupedAccounts(width: Int, name: String) throws {
        let preview = ProvidersPreview(width: CGFloat(width), overview: ProvidersFixture.tidyOverview(), report: nil)
        let renderer = ImageRenderer(content: preview.environment(\.colorScheme, .dark))
        renderer.scale = 2
        let image = try #require(renderer.nsImage)
        #expect(image.size.width == CGFloat(width))
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
    var overview = ProvidersFixture.overview()
    var report: UsageReport? = ProvidersFixture.report

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            PageHeader(title: "Providers", status: overview.status) {
                PaneButton(title: "Update All (\(overview.updatable.count))", symbol: "arrow.down.circle") {}
                PaneButton(title: "Add Account", symbol: "plus", prominent: true) {}
            }
            VStack(alignment: .leading, spacing: 0) {
                ProvidersPage(overview: overview, report: report, actions: ProvidersPage.Actions(
                    open: { _ in }, signIn: { _ in }, retry: { _ in }, fix: { _ in }, install: { _ in }, setUp: {},
                    addAccount: {}))
            }
            .pageColumn()
        }
        .frame(width: width)
        .background(Theme.canvas)
    }
}
#endif
