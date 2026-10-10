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

        /// The window with the least left, and its label: how close the login is to a limit.
        var tightest: (label: String, window: WindowLeft)? {
            [("Session", session), ("Weekly", weekly)]
                .compactMap { label, window in window.map { (label: label, window: $0) } }
                .min { $0.window.percentLeft < $1.window.percentLeft }
        }

        /// Plenty until a window reports otherwise.
        var headroom: Headroom { tightest?.window.headroom ?? .plenty }

        /// Whether the login stands out: near one of its limits, or missing on a machine.
        var needsAttention: Bool { headroom != .plenty || !missing.isEmpty }

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
            .map { Group(provider: $0.key, logins: $0.value.sorted(by: Self.attentionFirst)) }
            .sorted { $0.provider < $1.provider }
    }

    /// The logins that need attention first, then the closest to a limit, then by id.
    static func attentionFirst(_ a: Login, _ b: Login) -> Bool {
        if a.needsAttention != b.needsAttention { return a.needsAttention }
        let left = { (login: Login) in login.tightest?.window.percentLeft ?? .infinity }
        if left(a) != left(b) { return left(a) < left(b) }
        return a.id < b.id
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


/// Every provider login across the machines, in one place: how much of its plan's limits is
/// left, its tokens, cost, limit hits and failovers over a period, and where it is missing, to
/// set it up there: the provider's own login runs on that machine, after its installer when the
/// CLI is missing. A healthy login reads quietly; one near a limit or missing somewhere stands
/// out and comes first.
struct ProvidersView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    @State private var period = UsagePeriod.week
    @State private var setup: Setup?
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
            VStack(alignment: .leading, spacing: 24) {
                summary(accounts, report: report)
                ForEach(answer?.failures ?? [], id: \.self) { failure in
                    Text(failure).font(.footnote).foregroundStyle(Theme.failure)
                }
                if accounts.groups.isEmpty { empty }
                ForEach(accounts.groups) { group in
                    VStack(alignment: .leading, spacing: 10) {
                        HStack(spacing: 6) {
                            ProviderMark(provider: group.provider, size: 13).foregroundStyle(Theme.secondary)
                            SectionHeading(title: ModelCatalog.providerName(group.provider), count: group.logins.count)
                        }
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 300), spacing: 12, alignment: .top)],
                                  alignment: .leading, spacing: 12) {
                            ForEach(group.logins) { login in
                                card(login, usage: report?.logins[login.id], counted: report != nil)
                            }
                        }
                    }
                }
                if let report, !report.models.isEmpty { models(report.models) }
            }
            .padding(16)
        }
        .background(Theme.background)
        .navigationTitle("Providers")
        .toolbar {
            #if os(iOS)
            Button("Add Account", systemImage: "plus") { sheet = .addAccount }
            #endif
        }
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

    // MARK: Summary

    /// The period's cost and tokens over every machine, how many logins need attention, and
    /// the period picker.
    private func summary(_ accounts: ProviderAccounts, report: UsageReport?) -> some View {
        Card {
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .center, spacing: 16) {
                    totals(accounts, report: report)
                    Spacer(minLength: 16)
                    periodPicker.frame(width: 260)
                }
                VStack(alignment: .leading, spacing: 12) {
                    totals(accounts, report: report)
                    periodPicker
                }
            }
        }
    }

    private func totals(_ accounts: ProviderAccounts, report: UsageReport?) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            if let report {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(UsageReport.dollars(report.total)).font(.title2.weight(.semibold)).foregroundStyle(Theme.text)
                    Text("\(UsageReport.tokens(report.total.tokens)) tokens at API prices")
                        .font(.subheadline).foregroundStyle(Theme.secondary)
                }
                .monospacedDigit()
                .lineLimit(1)
                .accessibilityElement(children: .combine)
            } else {
                ProgressView().controlSize(.small).frame(height: 28)
            }
            let logins = accounts.groups.flatMap(\.logins)
            let attention = logins.filter(\.needsAttention).count
            HStack(spacing: 0) {
                Text("\(period.title) · \(logins.count == 1 ? "1 account" : "\(logins.count) accounts")")
                    .foregroundStyle(Theme.tertiary)
                if attention > 0 {
                    Text(" · \(attention) \(attention == 1 ? "needs" : "need") attention").foregroundStyle(Theme.accent)
                }
            }
            .font(.caption)
            .lineLimit(1)
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

    private var empty: some View {
        Card(padding: 20) {
            VStack(alignment: .leading, spacing: 12) {
                Image(systemName: "person.crop.circle.badge.plus").font(.title2).foregroundStyle(Theme.secondary)
                Text("No accounts yet").font(.headline).foregroundStyle(Theme.text)
                Text("Add a Claude, Codex, Cursor or OpenCode login on one of your machines. herder runs the provider’s own login there, in the account’s own config directory.")
                    .font(.subheadline).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                ActionButton(title: "Add Account", style: .primary) { sheet = .addAccount }.frame(maxWidth: 200)
            }
        }
    }

    // MARK: Account card

    /// One login: who it is and where, the headroom it has left, each window's meter, the
    /// period's cost and tokens, and badges only for what is worth a look.
    private func card(_ login: ProviderAccounts.Login, usage: UsageReport.Login?, counted: Bool) -> some View {
        let stroke = login.headroom == .plenty ? Theme.stroke : login.headroom.color.opacity(0.8)
        return VStack(alignment: .leading, spacing: 14) {
            identity(login)
            headroom(login)
            if login.session != nil || login.weekly != nil {
                Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 8) {
                    if let session = login.session { meter("Session", session) }
                    if let weekly = login.weekly { meter("Weekly", weekly) }
                }
            }
            footer(login, usage: usage, counted: counted)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(stroke))
    }

    private func identity(_ login: ProviderAccounts.Login) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(login.email ?? login.account.label).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    .lineLimit(1).truncationMode(.middle)
                if login.email == nil {
                    Text(login.account.accountId).font(Theme.monoSmall).foregroundStyle(Theme.tertiary).lineLimit(1)
                }
                Spacer(minLength: 4)
                let fallbackOn = login.fallbackOn
                if !fallbackOn.isEmpty {
                    // Machines may disagree: name the ones it is fallback-only on, unless all.
                    Text(fallbackOn.count == login.on.count
                         ? "Fallback" : "Fallback on \(fallbackOn.map(\.name).joined(separator: ", "))")
                        .font(.caption2.weight(.semibold)).foregroundStyle(Theme.secondary)
                        .padding(.horizontal, 6).padding(.vertical, 1)
                        .background(Theme.raised, in: .capsule)
                        .lineLimit(1)
                }
            }
            let sessions = login.sessions(in: fleet.lists.machines)
            Text(login.on.map(\.name).joined(separator: ", ")
                 + (sessions == 0 ? "" : sessions == 1 ? " · 1 session" : " · \(sessions) sessions"))
                .font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
        }
    }

    /// The headline: what is left of the tightest window.
    @ViewBuilder private func headroom(_ login: ProviderAccounts.Login) -> some View {
        if let tightest = login.tightest {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text("\(Int(tightest.window.percentLeft.rounded()))%")
                    .font(.title.weight(.semibold)).monospacedDigit()
                    .foregroundStyle(tightest.window.headroom == .plenty ? Theme.text : tightest.window.headroom.color)
                Text("left of the \(tightest.label.lowercased()) limit").font(.subheadline).foregroundStyle(Theme.secondary)
            }
            .lineLimit(1)
            .accessibilityElement(children: .combine)
        } else {
            Text("No plan limits reported yet").font(.subheadline).foregroundStyle(Theme.tertiary)
        }
    }

    private func meter(_ label: String, _ window: WindowLeft) -> some View {
        GridRow {
            Text(label).foregroundStyle(Theme.secondary)
            HeadroomMeter(window: window).frame(minWidth: 60, maxWidth: 140)
            Text("\(Int(window.percentLeft.rounded()))%").foregroundStyle(Theme.text).gridColumnAlignment(.trailing)
            Text(window.resets.isEmpty ? "" : "resets \(window.resets)").foregroundStyle(Theme.tertiary)
        }
        .font(.caption.monospacedDigit())
        .lineLimit(1)
        .accessibilityElement(children: .combine)
    }

    /// The period's cost and tokens together, then the badges.
    private func footer(_ login: ProviderAccounts.Login, usage: UsageReport.Login?, counted: Bool) -> some View {
        FlowLayout(spacing: 8, lineSpacing: 8) {
            if let usage, usage.amount.turns > 0 {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(UsageReport.dollars(usage.amount)).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    Text("\(UsageReport.tokens(usage.amount.tokens)) tokens").font(.caption).foregroundStyle(Theme.secondary)
                }
                .monospacedDigit()
                .padding(.trailing, 4)
            } else if counted {
                Text("No use \(period.label == "Month" ? "this month" : "in \(period.label)")")
                    .font(.caption).foregroundStyle(Theme.tertiary)
                    .padding(.trailing, 4)
            }
            if !login.missing.isEmpty { missing(login) }
            if let usage, usage.limitHits + usage.failoversOut + usage.failoversIn > 0 {
                let detail = "\(usage.limitHits) limit \(usage.limitHits == 1 ? "hit" : "hits"), "
                    + "failed over \(usage.failoversOut) out and \(usage.failoversIn) in"
                Chip(symbol: "arrow.triangle.swap",
                     text: usage.limitHits > 0 ? "\(usage.limitHits) limit \(usage.limitHits == 1 ? "hit" : "hits")"
                         : "\(usage.failoversOut + usage.failoversIn) failovers")
                    .help(detail)
                    .accessibilityLabel(detail)
            }
        }
    }

    /// The machines that lack the login: one badge, whose menu sets it up on each.
    private func missing(_ login: ProviderAccounts.Login) -> some View {
        Menu {
            ForEach(login.missing) { machine in
                if machine.owner {
                    Button(machine.needsInstall ? "Install and Set Up on \(machine.name)" : "Set Up on \(machine.name)") {
                        setUp(login, on: machine)
                    }
                    .disabled(!machine.connected)
                } else {
                    Button("\(machine.name): owners only") {}.disabled(true)
                }
            }
        } label: {
            Chip(symbol: "exclamationmark.circle",
                 text: login.missing.count == 1 ? "Missing on \(login.missing[0].name)"
                     : "Missing on \(login.missing.count) machines",
                 tint: Theme.accent)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Missing on \(login.missing.map(\.name).joined(separator: ", "))")
    }

    // MARK: Models

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
                .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
                .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
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

extension Headroom {
    /// Green with room to spare, amber getting low, vermilion nearly out; the light values are
    /// darker so they hold up on grey paper.
    var color: Color {
        switch self {
        case .plenty: Color(light: 0x2E7D5B, dark: 0x009E73)
        case .low: Color(light: 0x9A6700, dark: 0xE69F00)
        case .nearlyOut: Color(light: 0xB3401A, dark: 0xD55E00)
        }
    }
}

/// What is left of a limit window: a short track filled with the headroom, in its colour.
struct HeadroomMeter: View {
    let window: WindowLeft

    var body: some View {
        GeometryReader { geometry in
            ZStack(alignment: .leading) {
                Capsule().fill(Theme.raised)
                Capsule().fill(window.headroom.color)
                    .frame(width: geometry.size.width * min(window.percentLeft, 100) / 100)
            }
        }
        .frame(height: 6)
        .accessibilityHidden(true)
    }
}
