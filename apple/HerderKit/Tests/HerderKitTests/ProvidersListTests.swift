import Foundation
import Herder
@testable import HerderKit
import Testing

/// The Providers list P26.26 tidies: a row per login, grouped by provider and by where it
/// lives, named by its email, and renamed or removed from the list by its owner.
struct ProvidersListTests {
    private let now = Date(timeIntervalSince1970: 1_800_000_000)

    private func groups() -> [ProviderAccounts.Group] {
        ProviderAccounts(machines: ProvidersFixture.tidyMachines(now: now), now: now).groups
    }

    private func login(_ provider: Provider, _ title: String) throws -> ProviderAccounts.Login {
        try #require(groups().first { $0.provider == provider }?.logins.first { $0.title == title })
    }

    @Test func oneEmailOnSeveralMachinesIsOneRowListingThem() throws {
        let claude = try #require(groups().first { $0.provider == "claude" })
        // trash-can-01 has not probed its email yet; the label it was added under is it.
        let sam = claude.own.filter { $0.title == "sam@example.com" }
        #expect(sam.count == 1)
        #expect(sam.first?.machines == "studio-mac, trash-can-01, draugelis")
        #expect(sam.first?.subtitle == "Work")
        // A machine that spells the email in capitals is the same login.
        let shouting = Account(accountId: "x", provider: "claude", label: "x", configDir: nil, email: "SAM@Example.com",
                               usage: [])
        let quiet = Account(accountId: "y", provider: "claude", label: "y", configDir: nil, email: "sam@example.com",
                            usage: [])
        #expect(ProviderAccounts.key(shouting) == ProviderAccounts.key(quiet))
        // A label that is not an email says nothing of who it is.
        let named = Account(accountId: "z", provider: "claude", label: "Sam Work", configDir: nil, email: nil, usage: [])
        #expect(ProviderAccounts.identity(named) == nil)
    }

    @Test func hubLoginsSitApartFromEachMachinesOwn() throws {
        let claude = try #require(groups().first { $0.provider == "claude" })
        #expect(claude.throughHub.map(\.title).sorted()
                == ["Hub proxy", "sam.security@example.com", "sam@example.com", "spare@example.com"])
        #expect(claude.own.map(\.title).sorted() == ["sam.security@example.com", "sam@example.com"])
        #expect(ProvidersPage.hubHeading(claude.throughHub) == "Hub logins · home-hub")
        // The same email as a Codex login is a row of Codex's.
        let codex = try login("codex", "sam@example.com")
        #expect(codex.subtitle == "ChatGPT" && !codex.throughHub)
        #expect(groups().map(\.provider) == ["claude", "codex", "cursor"])
    }

    @Test func theHubsProxyIsNamedForWhatItIsNotItsUrlOrLabel() throws {
        let proxy = try login("claude", "Hub proxy")
        #expect(proxy.isHubProxy && proxy.throughHub && proxy.subtitle == nil)
        #expect(proxy.hub == ProviderAccounts.Hub(hostId: "hub", name: "home-hub", owner: true))
        #expect(!"\(proxy.title) \(proxy.subtitle ?? "")".contains("Vault"))
        #expect(!proxy.title.contains("http"))
        // Its URL is still the account page's.
        #expect(proxy.endpoint == "http://10.0.0.5:8317")
        // An endpoint on its own key keeps the label it was added under.
        var own = proxy.account
        own.keySecret = nil
        own.label = "Local model"
        let server = try #require(ProviderAccounts(machines: [machine("a", name: "a", sessions: [], accounts: [own])])
            .groups.first?.logins.first)
        #expect(server.title == "Local model" && !server.throughHub)
    }

    @Test func aHubLoginShowsItsEmailFirstAndItsLabelBeside() throws {
        let claude = try #require(groups().first { $0.provider == "claude" })
        let security = try #require(claude.throughHub.first { $0.hubLogin == "claude-sam.security@example.com.json" })
        #expect(security.title == "sam.security@example.com" && security.subtitle == "Sam Secutity")
        // A label that is its email shows once.
        let sam = try #require(claude.throughHub.first { $0.hubLogin == "claude-sam@example.com.json" })
        #expect(sam.title == "sam@example.com" && sam.subtitle == nil)
    }

    @Test func theSetUpHintIsOneLine() {
        let gaps = ProvidersFixture.tidyOverview(now: now).gaps
        #expect(!gaps.isEmpty)
        #expect(gaps.title == "\(gaps.gaps.count) accounts aren’t on every machine")
    }
}

@MainActor struct ProvidersListChangeTests {
    private let now = Date(timeIntervalSince1970: 1_800_000_000)

    private func login(_ machines: [Machine], where match: (ProviderAccounts.Login) -> Bool) throws -> ProviderAccounts.Login {
        let found = ProviderAccounts(machines: machines, now: now).groups.flatMap(\.logins).first(where: match)
        return try #require(found)
    }

    @Test func anOwnedHubsLoginIsChangedOnTheHub() throws {
        let machines = ProvidersFixture.tidyMachines(now: now)
        let sam = try login(machines) { $0.hubLogin == "claude-sam@example.com.json" }
        #expect(LoginTarget(sam, machines: machines) == .hubLogin(hub: "hub", name: "claude-sam@example.com.json"))
        #expect(LoginTarget.renamable(sam))
        // A member of the hub changes none of its logins.
        let member = hubLoginFleetMachines(member: true)
        let work = try login(member) { $0.hubLogin == "claude-ada@example.com.json" }
        #expect(LoginTarget(work, machines: member) == nil)
    }

    @Test func aMachinesLoginIsChangedOnTheMachinesTheUserOwns() throws {
        var machines = ProvidersFixture.tidyMachines(now: now)
        machines[2].role = .member
        let sam = try login(machines) { $0.title == "sam@example.com" && !$0.throughHub }
        guard case .accounts(let rows) = LoginTarget(sam, machines: machines) else {
            Issue.record("no accounts to change")
            return
        }
        #expect(rows.map(\.place.name) == ["studio-mac", "trash-can-01"])
        // The hub's proxy is removed from each machine, and has no label to rename.
        let proxy = try login(machines) { $0.isHubProxy }
        #expect(!LoginTarget.renamable(proxy))
        guard case .accounts(let proxies) = LoginTarget(proxy, machines: machines) else {
            Issue.record("no proxy to remove")
            return
        }
        #expect(proxies.count == 2)
    }

    @Test func removingAsksFirstAndWhichMachineWhenOnSeveral() throws {
        let machines = ProvidersFixture.tidyMachines(now: now)
        let hub = LoginTarget.hubLogin(hub: "hub", name: "claude-sam@example.com.json")
        #expect(hub.removalQuestion(title: "sam@example.com").title == "Remove sam@example.com from the hub?")
        #expect(hub.removalQuestion(title: "x").message.contains("drops the login and its tokens"))
        let sam = try login(machines) { $0.title == "sam@example.com" && !$0.throughHub }
        let target = try #require(LoginTarget(sam, machines: machines))
        #expect(target.removalQuestion(title: "sam@example.com").title == "Remove sam@example.com from which machine?")
        let security = try login(machines) { $0.title == "sam.security@example.com" && !$0.throughHub }
        let one = try #require(LoginTarget(security, machines: machines))
        #expect(one.removalQuestion(title: "sam.security@example.com").title
                == "Remove sam.security@example.com from trash-can-01?")
    }

    @Test func removingAHubLoginFromTheListDoesWhatTheLoginsTabDoes() async {
        let sent = Sent()
        let changes = LoginChanges { hostId, command in
            sent.append(hostId, command)
            if case .listProviderLogins = command { return .providerLogins(logins: []) }
            return .applied
        }
        let refused = await changes.remove(.hubLogin(hub: "hub", name: "claude-sam@example.com.json"))
        #expect(refused == nil)
        #expect(sent.hosts == ["hub", "hub"])
        #expect(sent.commands == [.removeProviderLogin(name: "claude-sam@example.com.json"), .listProviderLogins])
    }

    @Test func removingAMachinesLoginRemovesItsAccountFromTheChosenMachine() async throws {
        let machines = ProvidersFixture.tidyMachines(now: now)
        let sam = try login(machines) { $0.title == "sam@example.com" && !$0.throughHub }
        guard case .accounts(let rows) = LoginTarget(sam, machines: machines) else {
            Issue.record("no accounts")
            return
        }
        let sent = Sent()
        let changes = LoginChanges { hostId, command in
            sent.append(hostId, command)
            return .applied
        }
        // On several machines, nothing goes until one is chosen.
        #expect(await changes.remove(.accounts(rows)) == nil)
        #expect(sent.commands.isEmpty)
        #expect(await changes.remove(.accounts(rows), row: rows[1]) == nil)
        #expect(sent.hosts == ["can"])
        #expect(sent.commands == [.removeAccount(accountId: "claude")])
    }

    @Test func aRefusalIsShown() async throws {
        let changes = LoginChanges { _, _ in
            throw HerderError.Rejected(info: ErrorInfo(code: .conflict, message: "the proxy is busy"))
        }
        let why = await changes.remove(.hubLogin(hub: "hub", name: "claude-sam@example.com.json"))
        #expect(why?.contains("the proxy is busy") == true)
    }

    @Test func renamingAHubLoginNamesItOnTheHub() async {
        let sent = Sent()
        let changes = LoginChanges { hostId, command in
            sent.append(hostId, command)
            return .providerLogins(logins: [])
        }
        #expect(await changes.rename(.hubLogin(hub: "hub", name: "claude-sam.security@example.com.json"),
                                     to: " Sam Security ") == nil)
        #expect(sent.hosts == ["hub"])
        #expect(sent.commands == [.setProviderLogin(name: "claude-sam.security@example.com.json", label: "Sam Security",
                                                    fallback: nil)])
    }

    @Test func renamingAMachinesLoginLabelsItOnEachConnectedMachineOwned() async throws {
        var machines = ProvidersFixture.tidyMachines(now: now)
        machines[2].connection = .disconnected(error: "offline")
        let sam = try login(machines) { $0.title == "sam@example.com" && !$0.throughHub }
        let target = try #require(LoginTarget(sam, machines: machines))
        let sent = Sent()
        let changes = LoginChanges { hostId, command in
            sent.append(hostId, command)
            return .applied
        }
        #expect(await changes.rename(target, to: "Personal") == nil)
        #expect(sent.hosts == ["mac", "can"])
        let account = machines[0].accounts[0]
        #expect(sent.commands.first == account.settings(label: "Personal", configDir: nil, fallback: nil))
        // An empty label leaves a machine's login as it is.
        sent.clear()
        #expect(await changes.rename(target, to: "  ") == nil)
        #expect(sent.commands.isEmpty)
    }
}

/// What a `LoginChanges` sent, to which machine.
@MainActor private final class Sent {
    private(set) var hosts: [HostId] = []
    private(set) var commands: [CommandBody] = []

    func append(_ host: HostId, _ command: CommandBody) {
        hosts.append(host)
        commands.append(command)
    }

    func clear() {
        hosts = []
        commands = []
    }
}
