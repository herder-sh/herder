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
            AccountDraft(id: taken.contains(account.accountId)
                             ? AccountDraft.nextId(provider: provider, taken: taken, email: email) : account.accountId,
                         provider: provider, label: account.label,
                         configDir: account.configDir.flatMap { $0.hasPrefix("~/") ? $0 : nil } ?? "", idEdited: true)
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
            .map { window in
                let at = window.resetsAt.flatMap(Timestamp.date)
                return WindowLeft(percentUsed: window.usedPercent, resets: Timestamp.until(at, now: now), resetsAt: at)
            }
    }
}


/// Every provider login across the machines, in one place: one card per provider with its
/// accounts, how much of each plan's limits is left and what it cost over a period; the
/// provider CLIs' versions on every machine, to install or update them; and one way to set the
/// accounts up on the machines that lack them. A healthy login reads quietly; one near a limit
/// stands out.
struct ProvidersView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    @State private var period = UsagePeriod.week
    @State private var presented: Presented?
    @Environment(\.pages) private var pages
    @State private var showsModels = false

    /// An account to log in to on a machine.
    struct Setup: Identifiable {
        let hostId: HostId
        let draft: AccountDraft

        var id: HostId { hostId }
    }

    /// The one sheet the page shows: one that leads to another replaces itself with it.
    private enum Presented: Identifiable {
        case setUp
        case login(Setup)
        case add(Provider)

        var id: String {
            switch self {
            case .setUp: "set-up"
            case .login(let setup): "login/\(setup.hostId)/\(setup.draft.provider)"
            case .add(let provider): "add/\(provider)"
            }
        }
    }

    /// What the usage depends on: asked again when the period or the connected machines change.
    private struct Question: Equatable {
        let period: UsagePeriod
        let machines: [HostId]
    }

    var body: some View {
        let accounts = ProviderAccounts(machines: fleet.machines)
        let versions = ProviderVersions(machines: fleet.machines)
        let answer = fleet.usageCache[period]
        let report = answer.map { UsageReport($0.machines) }
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                summary(accounts, report: report)
                ForEach(answer?.failures ?? [], id: \.self) { failure in
                    Text(failure).font(.footnote).foregroundStyle(Theme.failure)
                }
                let gaps = SetUpGaps(accounts)
                if !gaps.isEmpty { banner(gaps) }
                if accounts.groups.isEmpty { empty }
                ForEach(accounts.groups) { group in
                    section(group, versions: versions.rows.first { $0.provider == group.provider }, report: report)
                }
                if !versions.rows.isEmpty { clis(versions) }
                if !fleet.mcpMachines.isEmpty { McpServersSection(fleet: fleet) }
                if let report, !report.models.isEmpty { models(report.models) }
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 16)
            .frame(maxWidth: 920, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
        .background(Theme.background)
        .navigationTitle("Providers")
        .toolbar {
            #if os(iOS)
            ActivityToolbarButton(fleet: fleet) { sheet = .activity }
            Button("Add Account", systemImage: "plus") { sheet = .addAccount }
            #endif
        }
        .refreshable { await fleet.refreshUsage(over: period, force: true) }
        .task(id: Question(period: period, machines: fleet.lists.machines
                .filter { $0.connected && $0.hosts.isEmpty }.map(\.hostId))) {
            await fleet.refreshUsage(over: period)
        }
        .sheet(item: $presented) { presented in
            switch presented {
            case .setUp: SetUpSheet(fleet: fleet) { self.presented = .login($0) }
            case .login(let setup):
                AddAccountSheet(fleet: fleet, hostId: setup.hostId, initialProvider: setup.draft.provider,
                                initialDraft: setup.draft)
            case .add(let provider): NewAccountSheet(fleet: fleet, provider: provider)
            }
        }
    }

    // MARK: Summary

    /// The period's cost and tokens over every machine, how many logins are near a limit, and
    /// the period picker.
    private func summary(_ accounts: ProviderAccounts, report: UsageReport?) -> some View {
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

    private func totals(_ accounts: ProviderAccounts, report: UsageReport?) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            if let report {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(UsageReport.dollars(report.total)).font(.title.weight(.semibold)).foregroundStyle(Theme.text)
                    Text("\(UsageReport.tokens(report.total.tokens)) tokens at API prices")
                        .font(.subheadline).foregroundStyle(Theme.secondary)
                }
                .monospacedDigit()
                .lineLimit(1)
                .accessibilityElement(children: .combine)
            } else {
                ProgressView().controlSize(.small).frame(height: 34)
            }
            let logins = accounts.groups.flatMap(\.logins)
            let low = logins.filter { $0.headroom != .plenty }.count
            HStack(spacing: 0) {
                Text("\(period.title) · \(logins.count == 1 ? "1 account" : "\(logins.count) accounts")")
                    .foregroundStyle(Theme.tertiary)
                if low > 0 {
                    Text(" · \(low) near a limit").foregroundStyle(Theme.accent)
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

    /// One line for every login missing on some machine, and one action to set them all up.
    private func banner(_ gaps: SetUpGaps) -> some View {
        HStack(alignment: .center, spacing: 12) {
            Image(systemName: "rectangle.stack.badge.plus").font(.title3).foregroundStyle(Theme.accent)
            VStack(alignment: .leading, spacing: 2) {
                Text(gaps.title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                Text(gaps.detail).font(.caption).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 12)
            PillButton(title: "Set Up…", prominent: true) { presented = .setUp }
        }
        .padding(14)
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
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

    // MARK: Provider

    /// A provider: its name, CLI version and a way to add an account, over its accounts.
    private func section(_ group: ProviderAccounts.Group, versions: ProviderVersions.Row?,
                         report: UsageReport?) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .center, spacing: 10) {
                ProviderMark(provider: group.provider, size: 20)
                Text(ModelCatalog.providerName(group.provider)).font(.headline).foregroundStyle(Theme.text)
                if let versions, let latest = versions.latest {
                    Text(latest).font(Theme.monoSmall).foregroundStyle(Theme.tertiary)
                    if let available = versions.available {
                        Text("Update available → \(available)").font(.caption).foregroundStyle(Theme.accent)
                    } else if !versions.outdated.isEmpty {
                        Text(versions.outdated.count == 1 ? "1 machine behind" : "\(versions.outdated.count) machines behind")
                            .font(.caption).foregroundStyle(Theme.accent)
                    }
                }
                Spacer(minLength: 8)
                PillButton(title: "Add", symbol: "plus") { presented = .add(group.provider) }
                    .accessibilityLabel("Add \(ModelCatalog.providerName(group.provider)) Account")
            }
            VStack(spacing: 0) {
                ForEach(Array(group.logins.enumerated()), id: \.element.id) { index, login in
                    if index > 0 { RowDivider() }
                    row(login, usage: report?.logins[login.id], counted: report != nil)
                }
            }
            .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
        }
    }

    /// One login: who it is and where on the left, what is left of its tightest limit and what
    /// it cost on the right; opens its details.
    private func row(_ login: ProviderAccounts.Login, usage: UsageReport.Login?, counted: Bool) -> some View {
        Button { pages.open(.account(loginId: login.id)) } label: {
            HStack(alignment: .center, spacing: 14) {
                VStack(alignment: .leading, spacing: 3) {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(login.email ?? login.account.label).font(.subheadline.weight(.semibold))
                            .foregroundStyle(Theme.text).lineLimit(1).truncationMode(.middle)
                        if !login.fallbackOn.isEmpty { Badge(text: "Fallback") }
                    }
                    Text(whereabouts(login, usage: usage)).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                }
                Spacer(minLength: 12)
                VStack(alignment: .trailing, spacing: 4) {
                    if let tightest = login.tightest {
                        HStack(alignment: .firstTextBaseline, spacing: 4) {
                            Text("\(Int(tightest.window.percentLeft.rounded()))%")
                                .font(.subheadline.weight(.semibold)).monospacedDigit()
                                .foregroundStyle(tightest.window.headroom == .plenty ? Theme.text : tightest.window.headroom.color)
                            Text("\(tightest.label.lowercased()) left").font(.caption).foregroundStyle(Theme.tertiary)
                        }
                        HeadroomMeter(window: tightest.window).frame(width: 96)
                    } else {
                        Text("No limits reported").font(.caption).foregroundStyle(Theme.tertiary)
                    }
                }
                .lineLimit(1)
                VStack(alignment: .trailing, spacing: 2) {
                    if let usage, usage.amount.turns > 0 {
                        Text(UsageReport.dollars(usage.amount)).font(.subheadline).foregroundStyle(Theme.text)
                        Text("\(UsageReport.tokens(usage.amount.tokens)) tokens").font(.caption).foregroundStyle(Theme.tertiary)
                    } else if counted {
                        Text("No use").font(.caption).foregroundStyle(Theme.tertiary)
                    }
                }
                .monospacedDigit()
                .lineLimit(1)
                .frame(minWidth: 72, alignment: .trailing)
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("account-row")
    }

    /// The label (when the email leads), the machines, the open sessions and the limit hits.
    private func whereabouts(_ login: ProviderAccounts.Login, usage: UsageReport.Login?) -> String {
        var parts: [String] = []
        if login.email != nil { parts.append(login.account.label) }
        parts.append(login.on.map(\.name).joined(separator: ", "))
        let sessions = login.sessions(in: fleet.lists.machines)
        if sessions > 0 { parts.append(sessions == 1 ? "1 session" : "\(sessions) sessions") }
        if let hits = usage?.limitHits, hits > 0 { parts.append(hits == 1 ? "1 limit hit" : "\(hits) limit hits") }
        return parts.joined(separator: " · ")
    }

    // MARK: CLIs

    /// Each provider CLI's version per machine, against the newest seen, and the way to bring
    /// every machine up to date.
    private func clis(_ versions: ProviderVersions) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Provider CLIs").font(.headline).foregroundStyle(Theme.text)
                    Text("Versions on each machine, against the newest any of them has and its vendor’s latest release.")
                        .font(.caption).foregroundStyle(Theme.secondary)
                }
                Spacer(minLength: 8)
                let updatable = versions.updatable
                if !updatable.isEmpty {
                    PillButton(title: "Update All (\(updatable.count))", symbol: "arrow.down.circle", prominent: true) {
                        for cell in updatable { fleet.install(cell.provider, on: cell.hostId) }
                    }
                } else if versions.rows.contains(where: { !$0.outdated.isEmpty }) {
                    PillButton(title: "Update All", symbol: "arrow.down.circle") {}
                        .disabled(true)
                        .help(versions.rows.flatMap(\.outdated).compactMap(\.blocked).first ?? "")
                }
            }
            VersionTable(versions: versions) { cell in
                guard cell.blocked == nil, cell.action != nil else { return }
                fleet.install(cell.provider, on: cell.hostId)
            }
        }
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
}

/// The logins missing on machines whose owner this device is: what the one set-up banner
/// counts, and the set-up sheet lists.
struct SetUpGaps: Equatable {
    struct Gap: Equatable, Identifiable {
        let login: ProviderAccounts.Login
        let machines: [ProviderAccounts.Place]

        var id: String { login.id }
    }

    let gaps: [Gap]

    init(_ accounts: ProviderAccounts) {
        gaps = accounts.groups.flatMap(\.logins).compactMap { login in
            let machines = login.missing.filter(\.owner)
            return machines.isEmpty ? nil : Gap(login: login, machines: machines)
        }
    }

    var isEmpty: Bool { gaps.isEmpty }

    var title: String {
        gaps.count == 1 ? "1 account isn’t on every machine" : "\(gaps.count) accounts aren’t on every machine"
    }

    /// The machines that lack one, by name.
    var detail: String {
        let names = gaps.flatMap { $0.machines.map(\.name) }.reduce(into: [String]()) { names, name in
            if !names.contains(name) { names.append(name) }
        }
        let list = names.count <= 1 ? names.joined()
            : names.dropLast().joined(separator: ", ") + " and " + names[names.count - 1]
        return "Set them up on \(list), so sessions there can use them and fail over to them."
    }
}

/// Every login missing on a machine, each with a way to set it up there: the provider's own
/// login on that machine, after its installer when the CLI is missing. Setting one up hands
/// it to `open`, which shows its login in this sheet's place.
struct SetUpSheet: View {
    let fleet: Fleet
    let open: (ProvidersView.Setup) -> Void

    var body: some View {
        let gaps = SetUpGaps(ProviderAccounts(machines: fleet.machines))
        SheetScaffold(title: "Set Up on Other Machines", subtitle: "Accounts some of your machines lack") {
            if gaps.isEmpty {
                Label("Every account is on every machine.", systemImage: "checkmark.circle")
                    .foregroundStyle(Theme.secondary)
            }
            ForEach(gaps.gaps) { gap in
                SettingsList(title: ModelCatalog.providerName(gap.login.provider)) {
                    HStack(spacing: 10) {
                        ProviderMark(provider: gap.login.provider, size: 16)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(gap.login.email ?? gap.login.account.label).font(.subheadline.weight(.semibold))
                                .foregroundStyle(Theme.text)
                            Text("On \(gap.login.on.map(\.name).joined(separator: ", "))").font(.caption)
                                .foregroundStyle(Theme.tertiary)
                        }
                        Spacer()
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    ForEach(gap.machines) { place in
                        RowDivider()
                        SetUpRow(fleet: fleet, login: gap.login, place: place, open: open)
                    }
                }
            }
        } footer: {
            Text("herder runs each provider’s own login on the machine, in the account’s config directory.")
                .font(.footnote).foregroundStyle(Theme.tertiary)
            Spacer()
        }
    }
}

/// A machine that lacks a login, and the button that sets it up there.
struct SetUpRow: View {
    let fleet: Fleet
    let login: ProviderAccounts.Login
    let place: ProviderAccounts.Place
    let open: (ProvidersView.Setup) -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "desktopcomputer").foregroundStyle(Theme.tertiary).frame(width: 16)
            Text(place.name).font(.subheadline).foregroundStyle(Theme.text)
            Spacer(minLength: 8)
            if !place.connected {
                Text("Not connected").font(.caption).foregroundStyle(Theme.tertiary)
            }
            PillButton(title: place.needsInstall ? "Install & Set Up" : "Set Up") {
                let taken = AccountDraft.taken(on: fleet.machines.first { $0.hostId == place.hostId },
                                               pending: fleet.activity.pendingIds(on: place.hostId))
                if place.needsInstall { fleet.install(login.provider, on: place.hostId) }
                open(ProvidersView.Setup(hostId: place.hostId, draft: login.draft(taken: taken)))
            }
            .disabled(!place.connected || !place.owner)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .frame(minHeight: 44)
    }
}

/// The provider CLIs as a table, a row per provider and a column per machine; on narrow
/// screens, a list per provider. `act` installs or updates one.
struct VersionTable: View {
    let versions: ProviderVersions
    let act: (ProviderVersions.Cell) -> Void

    var body: some View {
        let machines = versions.rows.first?.cells ?? []
        ViewThatFits(in: .horizontal) {
            table(machines).frame(minWidth: CGFloat(140 + machines.count * 150), maxWidth: .infinity, alignment: .leading)
            list
        }
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }

    private func table(_ machines: [ProviderVersions.Cell]) -> some View {
        Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 0) {
            GridRow {
                Text("")
                ForEach(machines) { Text($0.machine) }
            }
            .font(.caption.weight(.semibold))
            .foregroundStyle(Theme.tertiary)
            .padding(.vertical, 10)
            ForEach(versions.rows) { row in
                Divider().gridCellUnsizedAxes(.horizontal)
                GridRow {
                    HStack(spacing: 8) {
                        ProviderMark(provider: row.provider, size: 14)
                        Text(ModelCatalog.providerName(row.provider)).font(.subheadline).foregroundStyle(Theme.text)
                    }
                    ForEach(row.cells) { cell in VersionCell(cell: cell, act: act) }
                }
                .padding(.vertical, 10)
            }
        }
        .padding(.horizontal, 14)
    }

    private var list: some View {
        VStack(spacing: 0) {
            ForEach(Array(versions.rows.enumerated()), id: \.element.id) { index, row in
                if index > 0 { Divider() }
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 8) {
                        ProviderMark(provider: row.provider, size: 14)
                        Text(ModelCatalog.providerName(row.provider)).font(.subheadline.weight(.semibold))
                            .foregroundStyle(Theme.text)
                    }
                    ForEach(row.cells) { cell in
                        HStack {
                            Text(cell.machine).font(.caption).foregroundStyle(Theme.secondary)
                            Spacer()
                            VersionCell(cell: cell, act: act)
                        }
                    }
                }
                .padding(14)
            }
        }
    }
}

/// One CLI on one machine: its version, quiet when current, and the install or update herder
/// can run; why not, on hover, when it cannot.
struct VersionCell: View {
    let cell: ProviderVersions.Cell
    /// Whether to show the version an update brings; off where the row says it.
    var showsAvailable = true
    let act: (ProviderVersions.Cell) -> Void

    var body: some View {
        HStack(spacing: 6) {
            switch cell.state {
            case .missing:
                Text("Not installed").font(.caption).foregroundStyle(Theme.tertiary)
            case .outdated:
                Image(systemName: "arrow.up.circle.fill").font(.caption).foregroundStyle(Theme.accent)
                Text(cell.version ?? "").font(Theme.monoSmall).foregroundStyle(Theme.text)
                if showsAvailable, let available = cell.available {
                    Text("→ \(available)").font(Theme.monoSmall).foregroundStyle(Theme.accent)
                        .help("Update available → \(available)")
                }
            case .current, .unknown:
                Text(cell.version ?? "Installed").font(Theme.monoSmall).foregroundStyle(Theme.secondary)
            }
            if let action = cell.action {
                Button(action.title) { act(cell) }
                    .buttonStyle(.plain)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(cell.blocked == nil ? Theme.link : Theme.tertiary)
                    .disabled(cell.blocked != nil)
                    .help(cell.blocked ?? "\(action.title) \(ModelCatalog.providerName(cell.provider)) on \(cell.machine)")
            }
        }
        .lineLimit(1)
        .accessibilityElement(children: .combine)
    }
}

/// A small quiet capsule: fallback-only, and the like.
struct Badge: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.caption2.weight(.semibold)).foregroundStyle(Theme.secondary)
            .padding(.horizontal, 6).padding(.vertical, 1)
            .background(Theme.raised, in: .capsule)
            .lineLimit(1)
    }
}

/// A compact button for actions inside a card or section header; `prominent` for the one that
/// matters most there.
struct PillButton: View {
    let title: String
    var symbol: String?
    var prominent = false
    let action: () -> Void
    @Environment(\.isEnabled) private var enabled

    var body: some View {
        Button(action: action) {
            HStack(spacing: 4) {
                if let symbol { Image(systemName: symbol).imageScale(.small) }
                Text(title)
            }
            .font(.caption.weight(.semibold))
            .foregroundStyle(Theme.text)
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .primaryFill(in: .capsule, on: prominent)
            .opacity(enabled ? 1 : 0.45)
            .contentShape(.capsule)
            .fixedSize()
        }
        .buttonStyle(.plain)
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
