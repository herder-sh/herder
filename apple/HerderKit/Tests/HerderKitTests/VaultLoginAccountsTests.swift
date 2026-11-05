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

/// An account a host runs on one of its vault's logins: `vault-<pin>`, through the proxy.
func vaultLoginAccount(_ pin: String, name: String, label: String, email: String? = nil, fallback: Bool = false,
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
                   endpoint: "https://cpa.lan:8317", keySecret: AccountDraft.vaultKeySecret, vaultLogin: name)
}

/// Two hosts of the vault `h-vault`, each with its three logins as accounts and a login of its
/// own on the same email as one of them; the vault, owned unless `member`.
func vaultLoginFleetMachines(member: Bool = false, now: Date = .now) -> [Machine] {
    let logins = [
        vaultLoginAccount("work", name: "claude-ada@example.com.json", label: "Work", email: "ada@example.com",
                          session: 42, week: 81, now: now),
        vaultLoginAccount("ops", name: "claude-ops@example.com.json", label: "ops@example.com", email: "ops@example.com",
                          session: 12, week: 30, now: now),
        vaultLoginAccount("spare", name: "claude-spare.json", label: "Spare", fallback: true, now: now),
    ]
    let own = Account(accountId: "main", provider: "claude", label: "Main", configDir: nil, email: "ada@example.com",
                      usage: [UsageWindow(window: "five_hour", usedPercent: 5, resetsAt: nil)])
    let host = { (id: HostId) in FleetHost(hostId: id, hostName: id, online: true, lastSeen: now.ISO8601Format()) }
    return [
        machine("work-mbp", name: "work-mbp", sessions: [], accounts: [own] + logins, role: member ? .member : .owner),
        machine("trash-can-01", name: "trash-can-01", sessions: [], accounts: logins, role: member ? .member : .owner),
        machine("h-vault", name: "home-vault", sessions: [], hosts: [host("work-mbp"), host("trash-can-01")],
                role: member ? .member : .owner),
    ]
}

struct VaultLoginAccountsTests {
    @Test func eachVaultLoginIsItsOwnAccountUnderItsLabel() throws {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let logins = ProviderAccounts(machines: vaultLoginFleetMachines(now: now), now: now).groups.flatMap(\.logins)
        // Three vault logins on one endpoint, and the host's own login on Work's email: four.
        #expect(logins.count == 4)
        let work = try #require(logins.first { $0.id == ProviderAccounts.vaultKey(provider: "claude",
                                                                                      name: "claude-ada@example.com.json") })
        #expect(work.title == "Work" && work.subtitle == "ada@example.com")
        #expect(work.vaultLogin == "claude-ada@example.com.json" && work.endpoint == nil)
        #expect(work.on.map(\.name) == ["work-mbp", "trash-can-01"] && work.missing.isEmpty)
        #expect(work.session?.percentLeft == 58 && work.weekly?.percentLeft == 19)
        // A login its owner never named shows its email once.
        let ops = try #require(logins.first { $0.account.accountId == "vault-ops" })
        #expect(ops.title == "ops@example.com" && ops.subtitle == nil)
        let spare = try #require(logins.first { $0.account.accountId == "vault-spare" })
        #expect(spare.fallbackOn.count == 2 && spare.session == nil)
        let main = try #require(logins.first { $0.account.accountId == "main" })
        #expect(main.title == "ada@example.com" && main.subtitle == "Main" && main.vaultLogin == nil)
    }

    @Test func thePickerListsEachVaultLoginMarked() {
        let accounts = vaultLoginFleetMachines()[0].accounts
        let options = SettingsOption.accounts(accounts, current: "main")
        // Work shares the host login's email, yet is its own option.
        #expect(options.map(\.id) == ["main", "vault-work", "vault-ops", "vault-spare"])
        #expect(options.map(\.title) == ["Main", "Work", "ops@example.com", "Spare"])
        #expect(options.map(\.subtitle) == ["ada@example.com", "ada@example.com", nil, nil])
        #expect(options.map(\.badges) == [[], ["Vault"], ["Vault"], ["Vault", "Fallback"]])
        #expect(options[1].usage == 81 && options[1].detail == "Weekly 19% left")
    }

    @Test func pickingAVaultLoginSwitchesTheSessionToIt() {
        var chosen: [SettingsOption.ID] = []
        let section = SettingsSection(kind: .account,
                                      options: SettingsOption.accounts(vaultLoginFleetMachines()[0].accounts, current: "main")) {
            chosen.append($0)
        }
        section.perform(.option(.account, "vault-spare"))
        section.perform(.option(.account, "main"))
        #expect(chosen == ["vault-spare"])
    }

    @Test func aVaultLoginsPageFindsTheVaultOfItsHosts() {
        let machines = vaultLoginFleetMachines()
        #expect(AccountPage.vault(of: ["trash-can-01"], in: machines)?.hostId == "h-vault")
        #expect(AccountPage.vault(of: ["elsewhere"], in: machines) == nil)
        #expect(AccountPage.vaultNote(owner: true).contains("Logins tab"))
        #expect(!AccountPage.vaultNote(owner: false).contains("Logins tab"))
    }
}

/// Draws vault logins as the app's dark appearance shows them, on the Mac, and on an iPhone
/// simulator when the tests run there: the vault's Logins tab with usage, a fallback mark and a
/// refused rename; the account picker; the Providers page's accounts; and a vault login's
/// Account page, for an owner and a member. Attaches each; with `HERDER_SCREENSHOT_DIR` set,
/// also writes them there.
@MainActor struct VaultLoginScreenshotTests {
    #if os(macOS)
    private static let device = "mac"
    private static let pageWidth: CGFloat = 1040
    #else
    private static let device = "iphone"
    private static let pageWidth: CGFloat = 393
    #endif

    @Test func drawsTheLoginsTab() async throws {
        let fleet = try sampleFleet()
        let vault = FakeLoginsVault()
        let logins = vault.model()
        await logins.reload()
        await logins.rename(logins.logins[2], to: String(repeating: "x", count: 65))
        let page = VaultPage(fleet: fleet, vault: "h-vault", tab: .logins,
                             secrets: VaultSecrets(vault: "h-vault") { _ in .applied }, logins: logins)
        try await shoot(page, width: Self.pageWidth, height: Self.device == "mac" ? 520 : 760,
                        named: "vault-logins-\(Self.device).png")
    }

    @Test func drawsThePicker() async throws {
        let section = SettingsSection(kind: .account,
                                      options: SettingsOption.accounts(vaultLoginFleetMachines()[0].accounts,
                                                                       current: "vault-work")) { _ in }
        let menu = VStack(alignment: .leading, spacing: 0) {
            SettingsSectionRows(section: section, highlighted: .constant(nil), expanded: .constant(nil)) { _ in }
        }
        .padding(6)
        .frame(width: 340)
        .background(Theme.raised, in: .rect(cornerRadius: 10))
        .padding(20)
        try await shoot(menu, width: 380, height: 260, named: "vault-logins-picker-\(Self.device).png")
    }

    @Test func drawsTheProvidersAccounts() async throws {
        let machines = vaultLoginFleetMachines()
        let overview = ProvidersOverview(accounts: ProviderAccounts(machines: machines),
                                         versions: ProviderVersions(machines: machines), installs: [:])
        let page = ScrollView {
            ProvidersPage(overview: overview, report: nil, actions: ProvidersPage.Actions(
                open: { _ in }, signIn: { _ in }, retry: { _ in }, fix: { _ in }, install: { _ in }, setUp: {},
                addAccount: {}))
                .padding(20)
        }
        try await shoot(page, width: Self.pageWidth, height: 420, named: "vault-logins-providers-\(Self.device).png")
    }

    @Test(arguments: [false, true])
    func drawsAVaultLoginsAccountPage(member: Bool) async throws {
        let fleet = try sampleFleet(member: member)
        let page = VStack(spacing: 0) {
            PageTrail(crumbs: ["Providers", "Work"]) { _ in }
            AccountPage(fleet: fleet, loginId: ProviderAccounts.vaultKey(provider: "claude",
                                                                          name: "claude-ada@example.com.json"))
        }
        try await shoot(page, width: Self.pageWidth, height: Self.pageWidth > 500 ? 1000 : 1150,
                        named: "account-vault-login\(member ? "-member" : "")-\(Self.device).png")
    }

    private func sampleFleet(member: Bool = false) throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        fleet.setMachinesForTesting(vaultLoginFleetMachines(member: member))
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
