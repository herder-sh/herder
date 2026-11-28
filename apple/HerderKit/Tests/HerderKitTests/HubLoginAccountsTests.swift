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

/// An account a host runs on one of its hub's logins: `vault-<pin>`, through the proxy.
func hubLoginAccount(_ pin: String, name: String, label: String, email: String? = nil, fallback: Bool = false,
                       session: Double? = nil, week: Double? = nil, now: Date = .now) -> Account {
    var usage: [UsageWindow] = []
    if let session {
        usage.append(UsageWindow(window: "five_hour", usedPercent: session,
                                 resetsAt: now.addingTimeInterval(3 * 3600 + 10 * 60).ISO8601Format()))
    }
    if let week {
        usage.append(UsageWindow(window: "seven_day", usedPercent: week,
                                 resetsAt: now.addingTimeInterval(4 * 86_400).ISO8601Format()))
    }
    return Account(accountId: "vault-\(pin)", provider: "claude", label: label,
                   configDir: "~/.herder/accounts/vault-\(pin)", email: email, usage: usage, fallback: fallback,
                   endpoint: "https://cpa.lan:8317", keySecret: AccountDraft.hubKeySecret, hubLogin: name)
}

/// Two hosts of the hub `h-hub`, each with its three logins as accounts and a login of its
/// own on the same email as one of them; the hub, owned unless `member`.
func hubLoginFleetMachines(member: Bool = false, now: Date = .now) -> [Machine] {
    let logins = [
        hubLoginAccount("work", name: "claude-ada@example.com.json", label: "Work", email: "ada@example.com",
                          session: 42, week: 81, now: now),
        hubLoginAccount("ops", name: "claude-ops@example.com.json", label: "ops@example.com", email: "ops@example.com",
                          session: 12, week: 30, now: now),
        hubLoginAccount("spare", name: "claude-spare.json", label: "Spare", fallback: true, now: now),
    ]
    let own = Account(accountId: "main", provider: "claude", label: "Main", configDir: nil, email: "ada@example.com",
                      usage: [UsageWindow(window: "five_hour", usedPercent: 5, resetsAt: nil)])
    let host = { (id: HostId) in FleetHost(hostId: id, hostName: id, online: true, lastSeen: now.ISO8601Format()) }
    return [
        machine("work-mbp", name: "work-mbp", sessions: [], accounts: [own] + logins, role: member ? .member : .owner),
        machine("trash-can-01", name: "trash-can-01", sessions: [], accounts: logins, role: member ? .member : .owner),
        machine("h-hub", name: "home-hub", sessions: [], hosts: [host("work-mbp"), host("trash-can-01")],
                role: member ? .member : .owner),
    ]
}

struct HubLoginAccountsTests {
    @Test func eachHubLoginIsItsOwnAccountUnderItsLabel() throws {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let logins = ProviderAccounts(machines: hubLoginFleetMachines(now: now), now: now).groups.flatMap(\.logins)
        // Three hub logins on one endpoint, and the host's own login on Work's email: four.
        #expect(logins.count == 4)
        let work = try #require(logins.first { $0.id == ProviderAccounts.hubKey(provider: "claude",
                                                                                      name: "claude-ada@example.com.json") })
        // Its email first, as that is who it is; the hub owner's label beside it.
        #expect(work.title == "ada@example.com" && work.subtitle == "Work")
        #expect(work.throughHub && work.hub?.name == "home-hub")
        #expect(work.hubLogin == "claude-ada@example.com.json" && work.endpoint == nil)
        #expect(work.on.map(\.name) == ["work-mbp", "trash-can-01"] && work.missing.isEmpty)
        #expect(work.session?.percentLeft == 58 && work.weekly?.percentLeft == 19)
        // A login its owner never named shows its email once.
        let ops = try #require(logins.first { $0.account.accountId == "vault-ops" })
        #expect(ops.title == "ops@example.com" && ops.subtitle == nil)
        let spare = try #require(logins.first { $0.account.accountId == "vault-spare" })
        #expect(spare.fallbackOn.count == 2 && spare.session == nil)
        let main = try #require(logins.first { $0.account.accountId == "main" })
        #expect(main.title == "ada@example.com" && main.subtitle == "Main" && main.hubLogin == nil)
    }

    @Test func thePickerListsEachHubLoginMarked() {
        let accounts = hubLoginFleetMachines()[0].accounts
        let options = SettingsOption.accounts(accounts, current: "main")
        // Work shares the host login's email, yet is its own option.
        #expect(options.map(\.id) == ["main", "vault-work", "vault-ops", "vault-spare"])
        #expect(options.map(\.title) == ["Main", "Work", "ops@example.com", "Spare"])
        #expect(options.map(\.subtitle) == ["ada@example.com", "ada@example.com", nil, nil])
        #expect(options.map(\.badges) == [[], ["Hub"], ["Hub"], ["Hub", "Fallback"]])
        #expect(options[1].usage == 81 && options[1].detail == "Weekly 19% left")
    }

    @Test func pickingAHubLoginSwitchesTheSessionToIt() {
        var chosen: [SettingsOption.ID] = []
        let section = SettingsSection(kind: .account,
                                      options: SettingsOption.accounts(hubLoginFleetMachines()[0].accounts, current: "main")) {
            chosen.append($0)
        }
        section.perform(.option(.account, "vault-spare"))
        section.perform(.option(.account, "main"))
        #expect(chosen == ["vault-spare"])
    }

    @Test func aHubLoginsPageFindsTheHubOfItsHosts() {
        let machines = hubLoginFleetMachines()
        #expect(AccountPage.hub(of: ["trash-can-01"], in: machines)?.hostId == "h-hub")
        #expect(AccountPage.hub(of: ["elsewhere"], in: machines) == nil)
        #expect(AccountPage.hubNote(owner: true).contains("Logins tab"))
        #expect(!AccountPage.hubNote(owner: false).contains("Logins tab"))
    }
}

/// Draws hub logins as the app's dark appearance shows them, on the Mac, and on an iPhone
/// simulator when the tests run there: the hub's Logins tab with usage, a fallback mark and a
/// refused rename; the account picker; the Providers page's accounts; and a hub login's
/// Account page, for an owner and a member. Attaches each; with `HERDER_SCREENSHOT_DIR` set,
/// also writes them there.
@MainActor struct HubLoginScreenshotTests {
    #if os(macOS)
    private static let device = "mac"
    private static let pageWidth: CGFloat = 1040
    #else
    private static let device = "iphone"
    private static let pageWidth: CGFloat = 393
    #endif

    @Test func drawsTheLoginsTab() async throws {
        let fleet = try sampleFleet()
        let hub = FakeLoginsHub()
        let logins = hub.model()
        await logins.reload()
        await logins.rename(logins.logins[2], to: String(repeating: "x", count: 65))
        let page = HubPage(fleet: fleet, hub: "h-hub", tab: .logins,
                             secrets: HubSecrets(hub: "h-hub") { _ in .applied }, logins: logins)
        try await shoot(page, width: Self.pageWidth, height: Self.device == "mac" ? 520 : 760,
                        named: "hub-logins-\(Self.device).png")
    }

    @Test func drawsThePicker() async throws {
        let section = SettingsSection(kind: .account,
                                      options: SettingsOption.accounts(hubLoginFleetMachines()[0].accounts,
                                                                       current: "vault-work")) { _ in }
        let menu = VStack(alignment: .leading, spacing: 0) {
            SettingsSectionRows(section: section, highlighted: .constant(nil), expanded: .constant(nil)) { _ in }
        }
        .padding(6)
        .frame(width: 340)
        .background(Theme.raised, in: .rect(cornerRadius: 10))
        .padding(20)
        try await shoot(menu, width: 380, height: 260, named: "hub-logins-picker-\(Self.device).png")
    }

    @Test func drawsTheProvidersAccounts() async throws {
        let machines = hubLoginFleetMachines()
        let overview = ProvidersOverview(accounts: ProviderAccounts(machines: machines),
                                         versions: ProviderVersions(machines: machines), installs: [:])
        let page = ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                ProvidersPage(overview: overview, report: nil, actions: ProvidersPage.Actions(
                    open: { _ in }, signIn: { _ in }, retry: { _ in }, fix: { _ in }, install: { _ in }, setUp: {},
                    addAccount: {}))
            }
            .padding(20)
        }
        try await shoot(page, width: Self.pageWidth, height: 420, named: "hub-logins-providers-\(Self.device).png")
    }

    @Test(arguments: [false, true])
    func drawsAHubLoginsAccountPage(member: Bool) async throws {
        let fleet = try sampleFleet(member: member)
        let page = VStack(spacing: 0) {
            PageTrail(crumbs: ["Providers", "Work"]) { _ in }
            AccountPage(fleet: fleet, loginId: ProviderAccounts.hubKey(provider: "claude",
                                                                          name: "claude-ada@example.com.json"))
        }
        try await shoot(page, width: Self.pageWidth, height: Self.pageWidth > 500 ? 1000 : 1150,
                        named: "account-hub-login\(member ? "-member" : "")-\(Self.device).png")
    }

    private func sampleFleet(member: Bool = false) throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        fleet.setMachinesForTesting(hubLoginFleetMachines(member: member))
        return fleet
    }

    private func shoot(_ content: some View, width: CGFloat, height: CGFloat, named name: String) async throws {
        let root = content.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .background(Theme.canvas).environment(\.colorScheme, .dark)
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
}
