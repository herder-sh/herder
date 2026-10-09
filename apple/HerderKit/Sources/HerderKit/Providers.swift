import Herder
import SwiftUI

/// Each provider's logins across the machines: the accounts signed in as one email, or without
/// one the accounts with one id, the machines that have it and those that do not yet. A vault
/// runs no sessions, so has no accounts; a machine never connected has not said which it has.
struct ProviderAccounts: Equatable {
    struct Place: Equatable, Identifiable {
        let hostId: HostId
        let name: String
        let connected: Bool
        let owner: Bool
        /// Whether the provider's CLI is missing there and herder can install it.
        let needsInstall: Bool
        /// Whether the login there is fallback-only: picked only once its provider's other
        /// accounts there are unavailable. Never for a machine that lacks it.
        var fallback = false

        var id: HostId { hostId }
    }

    struct Login: Equatable, Identifiable {
        let provider: Herder.Provider
        let email: String?
        /// The first machine's account: its id, label and config dir are suggested elsewhere.
        let account: Herder.Account
        let on: [Place]
        let missing: [Place]

        var id: String { ProviderAccounts.key(account) }

        /// Where the login is fallback-only; machines may disagree.
        var fallbackOn: [Place] { on.filter(\.fallback) }

        /// The account to log in to on a machine that lacks it, whose accounts' ids are `taken`:
        /// the same id unless taken, label, and config dir when it is in the home dir, which
        /// names the same place there.
        func draft(taken: [String]) -> AccountDraft {
            var draft = AccountDraft(provider: provider, label: account.label,
                                     configDir: account.configDir.flatMap { $0.hasPrefix("~/") ? $0 : nil } ?? "")
            if taken.contains(account.accountId) {
                draft.fillId(taken: taken)
            } else {
                draft.id = account.accountId
            }
            return draft
        }
    }

    struct Group: Equatable, Identifiable {
        let provider: Herder.Provider
        let logins: [Login]

        var id: Herder.Provider { provider }
    }

    let groups: [Group]

    init(machines: [Machine]) {
        let daemons = machines.filter { $0.hosts.isEmpty && $0.role != nil }
        var firsts: [String: Herder.Account] = [:]
        for account in daemons.flatMap(\.accounts) where firsts[Self.key(account)] == nil {
            firsts[Self.key(account)] = account
        }
        let logins = firsts.map { id, account in
            let has = { (machine: Machine) in machine.accounts.contains { Self.key($0) == id } }
            let place = { (machine: Machine) in
                let status = machine.providers.first { $0.provider == account.provider }
                let here = machine.accounts.filter { Self.key($0) == id }
                return Place(hostId: machine.hostId, name: machine.name, connected: machine.connection == .connected,
                             owner: machine.role == .owner,
                             needsInstall: status?.installed == false && status?.canInstall == true,
                             fallback: !here.isEmpty && here.allSatisfy(\.fallback))
            }
            return Login(provider: account.provider, email: account.email, account: account,
                         on: daemons.filter(has).map(place), missing: daemons.filter { !has($0) }.map(place))
        }
        groups = Dictionary(grouping: logins, by: \.provider)
            .map { Group(provider: $0.key, logins: $0.value.sorted { $0.id < $1.id }) }
            .sorted { $0.provider < $1.provider }
    }

    /// What makes accounts one login: their provider and email, or without one their id.
    static func key(_ account: Herder.Account) -> String {
        "\(account.provider)/\(account.email ?? "id:\(account.accountId)")"
    }
}

/// Every provider login across the machines, and where each is missing, to set it up there:
/// the provider's own login runs on that machine, after its installer when the CLI is missing.
struct ProvidersView: View {
    let fleet: Fleet
    @State private var setup: Setup?

    /// An account to log in to on a machine.
    struct Setup: Identifiable {
        let hostId: HostId
        let draft: AccountDraft

        var id: HostId { hostId }
    }

    var body: some View {
        let accounts = ProviderAccounts(machines: fleet.machines)
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                if accounts.groups.isEmpty {
                    Text("No accounts yet. Add one in a machine's settings.").foregroundStyle(Theme.secondary)
                }
                ForEach(accounts.groups) { group in
                    SettingsGroup(title: ModelCatalog.providerName(group.provider)) {
                        ForEach(Array(group.logins.enumerated()), id: \.element.id) { index, login in
                            if index > 0 { RowDivider() }
                            row(login)
                        }
                    }
                }
            }
            .padding(16)
        }
        .background(Theme.background)
        .navigationTitle("Providers")
        .sheet(item: $setup) { setup in
            AddAccountSheet(fleet: fleet, hostId: setup.hostId, initialProvider: setup.draft.provider,
                            initialDraft: setup.draft)
        }
    }

    private func row(_ login: ProviderAccounts.Login) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(login.email ?? login.account.label).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                if login.email == nil {
                    Text(login.account.accountId).font(Theme.monoSmall).foregroundStyle(Theme.tertiary)
                }
                let fallbackOn = login.fallbackOn
                if !fallbackOn.isEmpty {
                    // Machines may disagree: name the ones it is fallback-only on, unless all.
                    Text(fallbackOn.count == login.on.count
                         ? "Fallback" : "Fallback on \(fallbackOn.map(\.name).joined(separator: ", "))")
                        .font(.caption2.weight(.semibold)).foregroundStyle(Theme.secondary)
                        .padding(.horizontal, 6).padding(.vertical, 2)
                        .background(Theme.raised, in: .capsule)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
                Label(login.on.map(\.name).joined(separator: ", "), systemImage: "checkmark.circle")
                    .font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
            }
            ForEach(login.missing) { machine in
                HStack(spacing: 8) {
                    Label("Missing on \(machine.name)", systemImage: "exclamationmark.circle")
                        .font(.caption).foregroundStyle(Theme.secondary)
                    Spacer(minLength: 8)
                    if machine.owner {
                        Button(machine.needsInstall ? "Install and Set Up" : "Set Up") { setUp(login, on: machine) }
                            .buttonStyle(.plain)
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(Theme.text)
                            .padding(.horizontal, 10)
                            .frame(height: 26)
                            .background(Theme.raised, in: .rect(cornerRadius: 6))
                            .disabled(!machine.connected)
                            .opacity(machine.connected ? 1 : 0.4)
                    } else {
                        Text("Owners only").font(.caption).foregroundStyle(Theme.tertiary)
                    }
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    /// Opens the add-account sheet on `machine` for `login`, running the provider's installer
    /// there first when its CLI is missing.
    private func setUp(_ login: ProviderAccounts.Login, on machine: ProviderAccounts.Place) {
        let taken = fleet.machines.first { $0.hostId == machine.hostId }?.accounts.map(\.accountId) ?? []
        if machine.needsInstall {
            fleet.accountLogins[machine.hostId] = TerminalConnection(
                hostId: machine.hostId, terminalId: nil, install: login.provider)
        }
        setup = Setup(hostId: machine.hostId, draft: login.draft(taken: taken))
    }
}
