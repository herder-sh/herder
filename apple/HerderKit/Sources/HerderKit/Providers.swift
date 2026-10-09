import Herder
import SwiftUI

/// Each provider's logins across the machines: the accounts signed in as one email, or without
/// one the accounts with one id, the machines that have it and those that do not yet, and what
/// is left of its plan's limits. A vault runs no sessions, so has no accounts; a machine never
/// connected has not said which it has.
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
        /// The login's accounts there; none for a machine that lacks it.
        var accounts: [AccountId] = []

        var id: HostId { hostId }
    }

    struct Login: Equatable, Identifiable {
        let provider: Herder.Provider
        let email: String?
        /// The first machine's account: its id, label and config dir are suggested elsewhere.
        let account: Herder.Account
        let on: [Place]
        let missing: [Place]
        /// The plan's session window (five hours), when the provider reports one.
        var session: WindowLeft?
        /// The plan's weekly window, when the provider reports one.
        var weekly: WindowLeft?

        var id: String { ProviderAccounts.key(account) }

        /// Where the login is fallback-only; machines may disagree.
        var fallbackOn: [Place] { on.filter(\.fallback) }

        /// The open sessions on the login's accounts, as `machines` count them.
        func sessions(in machines: [MachineSummary]) -> Int {
            on.reduce(0) { sum, place in
                let accounts = machines.first { $0.hostId == place.hostId }?.accounts ?? []
                return sum + accounts.filter { place.accounts.contains($0.accountId) }.map(\.sessions).reduce(0, +)
            }
        }

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

    init(machines: [Machine], now: Date = .now) {
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
                             fallback: !here.isEmpty && here.allSatisfy(\.fallback), accounts: here.map(\.accountId))
            }
            let windows = daemons.flatMap(\.accounts).filter { Self.key($0) == id }.flatMap(\.usage)
            return Login(provider: account.provider, email: account.email, account: account,
                         on: daemons.filter(has).map(place), missing: daemons.filter { !has($0) }.map(place),
                         session: Self.latest("Session", of: windows, now: now),
                         weekly: Self.latest("Weekly", of: windows, now: now))
        }
        groups = Dictionary(grouping: logins, by: \.provider)
            .map { Group(provider: $0.key, logins: $0.value.sorted { $0.id < $1.id }) }
            .sorted { $0.provider < $1.provider }
    }

    /// What makes accounts one login: their provider and email, or without one their id.
    static func key(_ account: Herder.Account) -> String {
        "\(account.provider)/\(account.email ?? "id:\(account.accountId)")"
    }

    /// Of the windows labelled `label`, the one that resets last, as the others are older
    /// reports of it; the more used one when they reset together.
    private static func latest(_ label: String, of windows: [UsageWindow], now: Date) -> WindowLeft? {
        let reset = { (window: UsageWindow) in window.resetsAt.flatMap(Timestamp.date) ?? .distantPast }
        return windows.filter { Lists.usageLabel($0.window) == label }
            .max { (reset($0), $0.usedPercent) < (reset($1), $1.usedPercent) }
            .map { WindowLeft(percentUsed: $0.usedPercent, resets: Timestamp.until($0.resetsAt.flatMap(Timestamp.date), now: now)) }
    }
}

/// Every provider login across the machines, in one place: what is left of its plan's limits,
/// its tokens, cost, limit hits and failovers over a period, and where it is missing, to set it
/// up there: the provider's own login runs on that machine, after its installer when the CLI is
/// missing.
struct ProvidersView: View {
    let fleet: Fleet
    @State private var period = UsagePeriod.week
    @State private var setup: Setup?
    /// The logins whose missing machines are listed, each with its Set Up button.
    @State private var expanded: Set<String> = []
    @State private var showsModels = false

    /// An account to log in to on a machine.
    struct Setup: Identifiable {
        let hostId: HostId
        let draft: AccountDraft

        var id: HostId { hostId }
    }

    /// What the usage depends on: asked again when the period or the connected machines change.
    private struct Question: Equatable {
        let period: UsagePeriod
        let machines: [HostId]
    }

    var body: some View {
        let accounts = ProviderAccounts(machines: fleet.machines)
        let answer = fleet.usageCache[period]
        let report = answer.map { UsageReport($0.machines) }
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                header(report)
                ForEach(answer?.failures ?? [], id: \.self) { failure in
                    Text(failure).font(.footnote).foregroundStyle(Theme.failure)
                }
                if accounts.groups.isEmpty {
                    Text("No accounts yet. Add one in a machine's settings.").foregroundStyle(Theme.secondary)
                }
                ForEach(accounts.groups) { group in
                    SettingsGroup(title: ModelCatalog.providerName(group.provider)) {
                        ForEach(Array(group.logins.enumerated()), id: \.element.id) { index, login in
                            if index > 0 { RowDivider() }
                            row(login, usage: report?.logins[login.id])
                        }
                    }
                }
                if let report, !report.models.isEmpty { models(report.models) }
            }
            .padding(16)
        }
        .background(Theme.background)
        .navigationTitle("Providers")
        .refreshable { await fleet.refreshUsage(over: period, force: true) }
        .task(id: Question(period: period, machines: fleet.lists.machines
                .filter { $0.connected && $0.hosts.isEmpty }.map(\.hostId))) {
            await fleet.refreshUsage(over: period)
        }
        .sheet(item: $setup) { setup in
            AddAccountSheet(fleet: fleet, hostId: setup.hostId, initialProvider: setup.draft.provider,
                            initialDraft: setup.draft)
        }
    }

    /// The period's cost and tokens over every machine, and the period picker.
    private func header(_ report: UsageReport?) -> some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 12) {
                totals(report)
                Spacer(minLength: 12)
                periodPicker.frame(maxWidth: 280)
            }
            VStack(alignment: .leading, spacing: 10) {
                periodPicker
                totals(report)
            }
        }
    }

    @ViewBuilder private func totals(_ report: UsageReport?) -> some View {
        if let report {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(UsageReport.dollars(report.total)).font(.title3.weight(.semibold)).foregroundStyle(Theme.text)
                Text("\(UsageReport.tokens(report.total.tokens)) tokens at API prices")
                    .font(.caption).foregroundStyle(Theme.secondary)
            }
            .monospacedDigit()
            .lineLimit(1)
            .accessibilityElement(children: .combine)
        } else {
            ProgressView().controlSize(.small)
        }
    }

    private var periodPicker: some View {
        Picker("Period", selection: $period) {
            ForEach(UsagePeriod.all, id: \.self) { Text($0.label).tag($0) }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .accessibilityLabel(period.title)
    }

    private func row(_ login: ProviderAccounts.Login, usage: UsageReport.Login?) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(login.email ?? login.account.label).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                    .lineLimit(1).truncationMode(.middle)
                if login.email == nil {
                    Text(login.account.accountId).font(Theme.monoSmall).foregroundStyle(Theme.tertiary).lineLimit(1)
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
                if let usage, usage.amount.turns > 0 {
                    Text(UsageReport.dollars(usage.amount)).font(.subheadline.weight(.semibold)).monospacedDigit()
                        .foregroundStyle(Theme.text).fixedSize()
                }
            }
            Label(detail(login, usage: usage), systemImage: "checkmark.circle")
                .font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
            if let session = login.session { window("Session", session) }
            if let weekly = login.weekly { window("Weekly", weekly) }
            if let usage, usage.limitHits + usage.failoversOut + usage.failoversIn > 0 {
                Label("\(usage.limitHits) limit \(usage.limitHits == 1 ? "hit" : "hits") · "
                      + "failed over \(usage.failoversOut) out, \(usage.failoversIn) in",
                      systemImage: "arrow.triangle.swap")
                    .font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
            }
            if !login.missing.isEmpty { missing(login) }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    /// Its machines, open sessions and the period's tokens.
    private func detail(_ login: ProviderAccounts.Login, usage: UsageReport.Login?) -> String {
        let sessions = login.sessions(in: fleet.lists.machines)
        var parts = [login.on.map(\.name).joined(separator: ", ")]
        if sessions > 0 { parts.append(sessions == 1 ? "1 session" : "\(sessions) sessions") }
        if let usage, usage.amount.turns > 0 { parts.append("\(UsageReport.tokens(usage.amount.tokens)) tokens") }
        return parts.joined(separator: " · ")
    }

    private func window(_ label: String, _ window: WindowLeft) -> some View {
        HStack(spacing: 10) {
            Text(label).frame(width: 56, alignment: .leading).foregroundStyle(Theme.secondary)
            UsageBar(percent: window.percentUsed)
            Text("\(Int(window.percentLeft.rounded()))% left").frame(width: 64, alignment: .trailing)
                .foregroundStyle(Theme.text)
            Text(window.resets.isEmpty ? "" : "resets \(window.resets)").frame(width: 92, alignment: .trailing)
                .foregroundStyle(Theme.tertiary)
        }
        .font(.caption.monospacedDigit())
        .lineLimit(1)
    }

    /// The machines that lack the login: one quiet line, which opens to a Set Up per machine.
    @ViewBuilder private func missing(_ login: ProviderAccounts.Login) -> some View {
        let open = expanded.contains(login.id)
        Button {
            if open { expanded.remove(login.id) } else { expanded.insert(login.id) }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: open ? "chevron.down" : "chevron.right").font(.caption2.weight(.semibold))
                Text("Missing on \(login.missing.map(\.name).joined(separator: ", "))").lineLimit(1)
            }
            .font(.caption)
            .foregroundStyle(Theme.tertiary)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        if open {
            ForEach(login.missing) { machine in
                HStack(spacing: 8) {
                    Label(machine.name, systemImage: "exclamationmark.circle")
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
                .padding(.leading, 16)
            }
        }
    }

    /// The period's tokens and dollars per model, folded away until asked for.
    private func models(_ rows: [UsageReport.ModelRow]) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Button { showsModels.toggle() } label: {
                HStack(spacing: 6) {
                    SectionHeading(title: "By Model", count: rows.count)
                    Image(systemName: showsModels ? "chevron.down" : "chevron.right")
                        .font(.caption2.weight(.semibold)).foregroundStyle(Theme.tertiary)
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            if showsModels {
                VStack(spacing: 0) {
                    ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                        if index > 0 { RowDivider() }
                        HStack(spacing: 10) {
                            ProviderMark(provider: row.provider, size: 13).frame(width: 16)
                            Text(row.model.isEmpty ? "Default" : row.model).font(Theme.monoSmall)
                                .lineLimit(1).truncationMode(.middle)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            Text(UsageReport.tokens(row.amount.tokens)).frame(width: 64, alignment: .trailing)
                            Text(UsageReport.dollars(row.amount)).frame(width: 72, alignment: .trailing)
                        }
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(Theme.text)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                    }
                }
                .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
                .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
            }
        }
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
