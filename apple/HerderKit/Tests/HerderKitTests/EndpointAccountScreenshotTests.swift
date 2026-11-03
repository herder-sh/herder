import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// Draws endpoint accounts as the app's dark appearance shows them, on the Mac, and on an iPhone
/// simulator when the tests run there: the Add Account sheet's choice between a login and an
/// endpoint, its endpoint form with the key typed hidden, and an endpoint account's page, which
/// shows the endpoint in place of limits and config directories. Attaches each; with
/// `HERDER_SCREENSHOT_DIR` set, also writes them there. Drawn in a window, as text fields are
/// the platform's, which `ImageRenderer` leaves out.
@MainActor struct EndpointAccountScreenshotTests {
    #if os(macOS)
    private static let device = "mac"
    private static let width: CGFloat = 560
    private static let pageWidth: CGFloat = 1040
    #else
    private static let device = "iphone"
    private static let width: CGFloat = 393
    private static let pageWidth: CGFloat = 393
    #endif

    @Test func drawsTheChoiceBetweenALoginAndAnEndpoint() async throws {
        try await shoot(AddEndpointPreview(endpoint: false), width: Self.width, height: 640,
                        named: "add-account-choice-\(Self.device).png")
    }

    @Test func drawsTheEndpointForm() async throws {
        try await shoot(AddEndpointPreview(endpoint: true), width: Self.width, height: Self.device == "mac" ? 780 : 1040,
                        named: "add-account-endpoint-\(Self.device).png")
    }

    @Test func drawsAnEndpointAccountsPage() async throws {
        let fleet = try sampleFleet()
        let page = VStack(spacing: 0) {
            PageTrail(crumbs: ["Providers", "Proxy"]) { _ in }
            AccountPage(fleet: fleet, loginId: ProviderAccounts.endpointKey(provider: "claude",
                                                                              endpoint: "http://127.0.0.1:8317"))
        }
        try await shoot(page, width: Self.pageWidth, height: Self.pageWidth > 500 ? 1250 : 1000,
                        named: "account-endpoint-\(Self.device).png")
    }

    private func shoot(_ content: some View, width: CGFloat, height: CGFloat, named name: String) async throws {
        let root = content.background(Theme.canvas).environment(\.colorScheme, .dark)
        #if os(macOS)
        let host = NSHostingView(rootView: root)
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(x: 0, y: 0, width: width, height: height)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        for _ in 0..<5 {
            host.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        #else
        let controller = UIHostingController(rootView: root)
        controller.overrideUserInterfaceStyle = .dark
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: width, height: height))
        window.overrideUserInterfaceStyle = .dark
        window.rootViewController = controller
        window.makeKeyAndVisible()
        for _ in 0..<5 {
            controller.view.layoutIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        let format = UIGraphicsImageRendererFormat()
        format.scale = 3
        // A test has no scene to show the window in: draw its layers.
        let image = UIGraphicsImageRenderer(bounds: window.bounds, format: format).image { context in
            controller.view.layer.render(in: context.cgContext)
        }
        let png = try #require(image.pngData())
        #endif
        Attachment.record(png, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }

    /// Two machines with the Claude endpoint account on the local CLIProxyAPI, and a member's
    /// machine that lacks it.
    private func sampleFleet() throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        func proxy(_ id: AccountId) -> Account {
            Account(accountId: id, provider: "claude", label: "Proxy", configDir: "~/.herder/accounts/\(id)",
                    email: nil, usage: [], endpoint: "http://127.0.0.1:8317")
        }
        let cli = ProviderStatus(provider: "claude", installed: true, version: "2.1.310 (Claude Code)",
                                 latestVersion: nil, binary: nil, canInstall: true, canUpdate: true)
        fleet.setMachinesForTesting([
            machine("trash-can-01", name: "trash-can-01", sessions: ["01A"], accounts: [proxy("claude-proxy")],
                    providers: [cli]),
            machine("work-mbp", name: "work-mbp", sessions: [], accounts: [proxy("claude-proxy")], providers: [cli]),
            machine("draugelis", name: "draugelis", sessions: [], providers: [cli]),
        ])
        return fleet
    }
}

/// The Add Account sheet, with Claude picked, two machines ticked and a member's machine
/// disabled; on an endpoint, its URL and key typed.
private struct AddEndpointPreview: View {
    let endpoint: Bool
    @State private var draft = AccountDraft(provider: "claude", label: "Proxy")
    @State private var ticked: Set<HostId> = ["work-mbp", "trash-can-01"]
    @State private var showsConfigDir = false

    init(endpoint: Bool) {
        self.endpoint = endpoint
        var draft = AccountDraft(provider: "claude", label: endpoint ? "Proxy" : "work")
        draft.endpoint = endpoint
        if endpoint {
            draft.url = "http://127.0.0.1:8317"
            draft.key = "sk-proxy-0123456789"
        }
        _draft = State(initialValue: draft)
    }

    var body: some View {
        SheetScaffold(title: "Add Account", height: endpoint ? 1040 : 640, hint: AddAccountSheet.hint(endpoint: endpoint)) {
            AddAccountForm(draft: $draft, ticked: $ticked, showsConfigDir: $showsConfigDir, targets: [
                AccountTarget(hostId: "work-mbp", name: "work-mbp", problem: nil),
                AccountTarget(hostId: "trash-can-01", name: "trash-can-01", problem: nil),
                AccountTarget(hostId: "draugelis", name: "draugelis", problem: "Member — can’t add accounts", owner: false),
            ], installing: { _ in false }, install: { _ in },
               failures: endpoint ? ["trash-can-01": "endpoint accounts are not supported by this daemon"] : [:])
        } footer: {
            Spacer()
            ActionButton(title: AddAccountSheet.title(ticked.count, endpoint: endpoint), style: .primary) {}
                .frame(width: 220)
        }
    }
}
