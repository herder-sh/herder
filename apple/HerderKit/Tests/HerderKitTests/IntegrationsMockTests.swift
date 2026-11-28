import Foundation
import Herder
@testable import HerderKit
import Testing

/// The Integrations mock (P30.1) is the demo's alone: no real fleet lists it.
@MainActor struct IntegrationsMockTests {
    @Test func onlyTheDemoIsADemo() throws {
        #expect(try Fleet.demo().isDemo)
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        #expect(!fleet.isDemo)
    }

    @Test func theCatalogHasBothHalvesAndEveryPageAName() throws {
        #expect(!MockIntegrations.installed.isEmpty)
        #expect(!MockIntegrations.available.isEmpty)
        #expect(Set(MockIntegrations.catalog.map(\.id)).count == MockIntegrations.catalog.count)
        #expect(MockIntegrations.status == "4 installed · 1 needs you · 1 failing · 8 available")
        let fleet = try Fleet.demo()
        #expect(Page.integration("io.sentry.mcp").title(in: fleet) == "Sentry")
        #expect(Page.integrations.title(in: fleet) == "Integrations")
        #expect(CompactTab(.integrations) == .hub)
    }
}

#if os(macOS)
import AppKit
import SwiftUI

/// Draws the Integrations mock on the demo fleet: the catalog in the Mac window with the sidebar
/// open, an installed integration's page (GitHub), a failing one's (Sentry), one not installed
/// (Linear), and the install sheet. Attaches each; with `HERDER_SCREENSHOT_DIR` set, also writes
/// them there as `integrations-*.png`.
@MainActor @Suite(.serialized) struct IntegrationsScreenshotTests {
    @Test func drawsTheCatalog() async throws {
        try await shoot(window(.integrations), size: CGSize(width: 1432, height: 900), named: "integrations-mac.png")
    }

    @Test func drawsTheCatalogInANarrowWindow() async throws {
        try await shoot(window(.integrations), collapsed: true, size: CGSize(width: 900, height: 900),
                        named: "integrations-mac-narrow.png")
    }

    @Test func drawsAnInstalledIntegration() async throws {
        try await shoot(window(.integrations, page: "sh.herder.github"), size: CGSize(width: 1432, height: 1100),
                        named: "integrations-page-github-mac.png")
    }

    @Test func drawsAFailingIntegration() async throws {
        try await shoot(window(.integrations, page: "io.sentry.mcp"), size: CGSize(width: 1432, height: 1000),
                        named: "integrations-page-sentry-mac.png")
    }

    @Test func drawsAnIntegrationNotInstalled() async throws {
        try await shoot(window(.integrations, page: "app.linear"), size: CGSize(width: 1432, height: 800),
                        named: "integrations-page-linear-mac.png")
    }

    @Test func drawsTheInstallSheet() async throws {
        let fleet = try Fleet.demo()
        try await shoot(InstallIntegrationSheet(fleet: fleet, integrationId: "app.linear"),
                        size: CGSize(width: 560, height: 620), named: "integrations-install-mac.png")
    }

    private func window(_ item: SidebarItem, page: String? = nil) throws -> some View {
        let fleet = try Fleet.demo()
        var pages = PageStack()
        if let page { pages.open(.integration(page)) }
        return DesktopShell(fleet: fleet, sheet: .constant(nil), item: .constant(item), pages: .constant(pages),
                            session: .constant(nil), draft: .constant(nil), people: FleetPeople(fleet: fleet),
                            opened: { _ in }, perform: { _ in })
    }

    private func shoot(_ content: some View, collapsed: Bool = false, size: CGSize, named name: String) async throws {
        UserDefaults.standard.set(collapsed, forKey: "sidebarCollapsed")
        defer { UserDefaults.standard.removeObject(forKey: "sidebarCollapsed") }
        let view = content
            .frame(width: size.width, height: size.height)
            .background(Theme.canvas)
            .environment(\.colorScheme, .dark)
        let host = NSHostingView(rootView: view)
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(origin: .zero, size: size)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        // A page shows its index once it has read its sections.
        for _ in 0..<8 {
            host.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
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
