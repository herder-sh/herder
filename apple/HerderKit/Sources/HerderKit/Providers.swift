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
        /// Those of them the daemon found signed out, and when the first was.
        var signedOut: [AccountId] = []
        var signedOutSince: Date?

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

        /// The machines it is on, for a cell: the one's name, or how many.
        var machines: String { on.count == 1 ? on.map(\.name).joined() : "\(on.count) machines" }

        /// Whether it is signed out on every machine it is on.
        var signedOutEverywhere: Bool { !on.isEmpty && on.allSatisfy { !$0.signedOut.isEmpty } }

        /// Whether the login stands out: near one of its limits, signed out or missing on a
        /// machine.
        var needsAttention: Bool { headroom != .plenty || !missing.isEmpty || on.contains { !$0.signedOut.isEmpty } }

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
                             ? AccountDraft.nextId(provider: provider, taken: taken, name: email) : account.accountId,
                         provider: provider, label: account.label,
                         configDir: account.configDir.flatMap { $0.hasPrefix("~/") ? $0 : nil } ?? "")
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
                let signedOut = here.filter { $0.needsLogin != nil }
                return Place(hostId: machine.hostId, name: machine.name, connected: machine.connection == .connected,
                             owner: machine.role == .owner,
                             needsInstall: status?.installed == false && status?.canInstall == true,
                             fallback: !here.isEmpty && here.allSatisfy(\.fallback), accounts: here.map(\.accountId),
                             signedOut: signedOut.map(\.accountId),
                             signedOutSince: signedOut.compactMap { $0.needsLogin.flatMap(Timestamp.date) }.min())
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
                // A reset already past says nothing: the next report brings the new one.
                let at = window.resetsAt.flatMap(Timestamp.date).flatMap { $0 > now ? $0 : nil }
                return WindowLeft(percentUsed: window.usedPercent, resets: Timestamp.until(at, now: now), resetsAt: at)
            }
    }
}


/// The Providers page's state: its accounts and CLI versions, the CLI installs this device
/// runs, what needs the user, and the header's status sentence.
struct ProvidersOverview {
    /// An account signed out on a machine: its sessions there fail until it signs in again.
    struct SignedOut: Identifiable {
        let login: ProviderAccounts.Login
        let place: ProviderAccounts.Place
        let accountId: AccountId

        var id: String { "\(place.hostId)/\(accountId)" }
    }

    /// An install or update this device ran that failed.
    struct FailedInstall: Identifiable {
        let key: JobKey
        let provider: Provider
        let machine: String
        let message: String
        /// The installer's last lines of output, for the card's details.
        var output = ""

        var id: JobKey { key }
    }

    let accounts: ProviderAccounts
    let versions: ProviderVersions
    /// The CLI installs and updates this device runs or ran, by job.
    let installs: [JobKey: JobProgress]
    let signedOut: [SignedOut]
    let failedInstalls: [FailedInstall]
    /// The failed installs Fix with AI is offered for.
    let fixable: Set<JobKey>
    let gaps: SetUpGaps

    init(accounts: ProviderAccounts, versions: ProviderVersions, installs: [JobKey: JobProgress],
         fixable: Set<JobKey> = [], outputs: [JobKey: String] = [:]) {
        self.accounts = accounts
        self.versions = versions
        self.installs = installs
        self.fixable = fixable
        signedOut = accounts.groups.flatMap(\.logins).flatMap { login in
            login.on.flatMap { place in
                place.signedOut.map { SignedOut(login: login, place: place, accountId: $0) }
            }
        }
        let machines = Dictionary(versions.rows.flatMap(\.cells).map { ($0.hostId, $0.machine) }) { first, _ in first }
        failedInstalls = installs.compactMap { key, progress in
            guard case .failed(let message) = progress, case .install(let provider) = key.kind else { return nil }
            return FailedInstall(key: key, provider: provider, machine: machines[key.hostId] ?? key.hostId, message: message,
                                 output: outputs[key] ?? "")
        }
        .sorted { ($0.machine, $0.provider) < ($1.machine, $1.provider) }
        gaps = SetUpGaps(accounts)
    }

    @MainActor init(fleet: Fleet) {
        let keys = fleet.activity.keys.filter(\.isInstall)
        let installs = keys.map { ($0, fleet.progress($0)) }
        let failed = keys.filter { fleet.progress($0).failed }
        self.init(accounts: ProviderAccounts(machines: fleet.machines), versions: ProviderVersions(machines: fleet.machines),
                  installs: Dictionary(installs) { _, last in last }, fixable: Set(failed.filter(fleet.canFix)),
                  outputs: Dictionary(failed.map { ($0, fleet.failureOutput($0)) }) { _, last in last })
    }

    /// The install job for `cell`, while this device runs or keeps one.
    func job(_ cell: ProviderVersions.Cell) -> JobProgress? {
        installs[JobKey(hostId: cell.hostId, kind: .install(cell.provider))]
    }

    /// The CLIs herder can update now that no job of this device is updating already.
    var updatable: [ProviderVersions.Cell] {
        versions.updatable.filter { job($0).map(\.finished) ?? true }
    }

    /// Whether something stops sessions until the user acts; an account missing on a machine
    /// does not, so the page notes it with the accounts instead.
    var needsAttention: Bool { !signedOut.isEmpty || !failedInstalls.isEmpty }

    /// "5 accounts · 1 needs sign-in · 3 updates"; the counts that are zero say nothing.
    var status: String {
        let logins = accounts.groups.flatMap(\.logins).count
        var parts = [logins == 1 ? "1 account" : "\(logins) accounts"]
        if !signedOut.isEmpty { parts.append(signedOut.count == 1 ? "1 needs sign-in" : "\(signedOut.count) need sign-in") }
        let updates = updatable.count
        if updates > 0 { parts.append(updates == 1 ? "1 update" : "\(updates) updates") }
        return parts.joined(separator: " · ")
    }
}

extension Fleet {
    /// Queues updating each of `cells`' CLIs on its machine.
    func update(_ cells: [ProviderVersions.Cell]) {
        for cell in cells { install(cell.provider, on: cell.hostId) }
    }
}

/// Every provider login across the machines, in one place: what needs the user first, each
/// with its fix; then the accounts, how much of each plan's limits is left and what it cost
/// over the last week; then the provider CLIs' versions on every machine, with the installs
/// running in their cells. A healthy account or a current CLI reads quietly.
struct ProvidersView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    @State private var presented: Presented?
    @Environment(\.pages) private var pages
    @State private var showsModels = false
    private let period = UsagePeriod.week

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

        var id: String {
            switch self {
            case .setUp: "set-up"
            case .login(let setup): "login/\(setup.hostId)/\(setup.draft.provider)"
            }
        }
    }

    var body: some View {
        let overview = ProvidersOverview(fleet: fleet)
        let answer = fleet.usageCache[period]
        let report = answer.map { UsageReport($0.machines) }
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                #if os(iOS)
                Text(overview.status).font(Theme.body).foregroundStyle(Theme.secondary)
                #endif
                ForEach(answer?.failures ?? [], id: \.self) { failure in
                    Text(failure).font(.footnote).foregroundStyle(Theme.failure)
                }
                ProvidersPage(overview: overview, report: report, actions: ProvidersPage.Actions(
                    open: { pages.open(.account(loginId: $0.id)) },
                    signIn: { fleet.relogin($0.accountId, on: $0.place.hostId) },
                    retry: { fleet.retry($0) },
                    fix: { key in Task { await fleet.fix(key) } },
                    install: { fleet.install($0.provider, on: $0.hostId) },
                    setUp: { presented = .setUp },
                    addAccount: { sheet = .addAccount }))
                HerderVersionsSection(fleet: fleet)
                if !fleet.mcpMachines.isEmpty { McpServersSection(fleet: fleet) }
                if let report, !report.models.isEmpty { models(report.models) }
            }
            .pageColumn()
        }
        .background(Theme.background)
        .navigationTitle("Providers")
        .toolbar {
            #if os(iOS)
            ActivityToolbarButton(fleet: fleet) { sheet = .activity }
            let updatable = overview.updatable
            if !updatable.isEmpty {
                Button("Update All (\(updatable.count))", systemImage: "arrow.down.circle") { fleet.update(updatable) }
            }
            Button("Add Account", systemImage: "plus") { sheet = .addAccount }
            #endif
        }
        .refreshable { await fleet.refreshUsage(over: period, force: true) }
        .task(id: fleet.lists.machines.filter { $0.connected && $0.hosts.isEmpty }.map(\.hostId)) {
            await fleet.refreshUsage(over: period)
        }
        .sheet(item: $presented) { presented in
            switch presented {
            case .setUp: SetUpSheet(fleet: fleet) { self.presented = .login($0) }
            case .login(let setup):
                AddAccountSheet(fleet: fleet, hostId: setup.hostId, initialProvider: setup.draft.provider,
                                initialDraft: setup.draft)
            }
        }
    }

    /// The week's tokens and dollars per model, folded away until asked for.
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
                        .padding(.horizontal, Theme.cellPadding)
                        .padding(.vertical, 10)
                    }
                }
                .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
                .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
            }
        }
    }
}

/// The Providers page's sections, from its overview and the week's usage: what needs the user,
/// the accounts table and the CLI versions matrix.
struct ProvidersPage: View {
    /// What the page's buttons do.
    struct Actions {
        let open: (ProviderAccounts.Login) -> Void
        let signIn: (ProvidersOverview.SignedOut) -> Void
        let retry: (JobKey) -> Void
        /// Starts a session on the machine that fixes a failed install.
        let fix: (JobKey) -> Void
        let install: (ProviderVersions.Cell) -> Void
        let setUp: () -> Void
        let addAccount: () -> Void
    }

    /// An account's row in the table, with its week's cost once the machines have said it.
    struct AccountRow: Identifiable {
        let login: ProviderAccounts.Login
        let usage: UsageReport.Login?

        var id: String { login.id }
    }

    let overview: ProvidersOverview
    let report: UsageReport?
    let actions: Actions
    /// The failed installs whose output is shown.
    @State private var details: Set<JobKey> = []

    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            if overview.needsAttention { attention }
            let logins = overview.accounts.groups.flatMap(\.logins)
            if logins.isEmpty {
                empty
            } else {
                ProvidersSection(title: "Accounts", count: logins.count) {
                    let rows = logins.map { AccountRow(login: $0, usage: report?.logins[$0.id]) }
                    ViewThatFits(in: .horizontal) {
                        accounts(rows, narrow: false).frame(minWidth: ProvidersColumns.wideTable)
                        accounts(rows, narrow: true)
                    }
                    if !overview.gaps.isEmpty { gaps }
                }
            }
            if !overview.versions.rows.isEmpty {
                ProvidersSection(title: "CLI versions", note: "latest from each vendor") {
                    VersionMatrix(overview: overview, act: actions.install, retry: actions.retry, fix: actions.fix)
                }
            }
        }
    }

    // MARK: Needs you

    /// Signed-out accounts, failed installs and accounts missing on machines, each with its fix.
    private var attention: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: "Needs you", tint: Theme.accent)
            ForEach(overview.signedOut) { signedOut in
                let name = "\(ModelCatalog.providerName(signedOut.login.provider)) · \(signedOut.login.email ?? signedOut.login.account.label)"
                problem(urgency: .needsYou, title: "\(name) is signed out on \(signedOut.place.name)") {
                    if let since = signedOut.place.signedOutSince {
                        // One text, so it wraps as one line does.
                        Text("Sessions there fail until it signs in · signed out \(since, format: .relative(presentation: .named, unitsStyle: .abbreviated))")
                    } else {
                        Text("Sessions there fail until it signs in")
                    }
                } fix: {
                    PillButton(title: "Sign In Again", prominent: true) { actions.signIn(signedOut) }
                        .disabled(!signedOut.place.owner || !signedOut.place.connected)
                        .help(!signedOut.place.owner ? "Only the machine’s owner can sign in on it."
                              : signedOut.place.connected ? "" : "\(signedOut.place.name) is not connected.")
                }
            }
            ForEach(overview.failedInstalls) { failed in
                problem(urgency: .failing, title: "\(ModelCatalog.providerName(failed.provider)) failed to install on \(failed.machine)") {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(failed.message)
                        if !failed.output.isEmpty {
                            Button(details.contains(failed.key) ? "Hide details" : "Details") {
                                if details.contains(failed.key) { details.remove(failed.key) } else { details.insert(failed.key) }
                            }
                            .buttonStyle(.plain).foregroundStyle(Theme.text)
                        }
                    }
                } fix: {
                    HStack(spacing: 8) {
                        if overview.fixable.contains(failed.key) {
                            PillButton(title: "Fix with AI", symbol: "sparkles") { actions.fix(failed.key) }
                        }
                        PillButton(title: "Retry", prominent: true) { actions.retry(failed.key) }
                    }
                } more: {
                    if details.contains(failed.key) { FailureOutput(text: failed.output) }
                }
            }
        }
    }

    /// The accounts some machines lack, under the table: worth setting up, not urgent.
    private var gaps: some View {
        HStack(alignment: .center, spacing: 10) {
            Image(systemName: "info.circle").foregroundStyle(Theme.secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(overview.gaps.title).font(Theme.body).foregroundStyle(Theme.text)
                    .fixedSize(horizontal: false, vertical: true)
                Text(overview.gaps.detail).font(.caption).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 12)
            PillButton(title: "Set Up…") { actions.setUp() }
        }
        .padding(.vertical, 10)
    }

    private func problem<Detail: View, Fix: View, More: View>(
        urgency: Urgency, title: String, @ViewBuilder detail: () -> Detail, @ViewBuilder fix: () -> Fix,
        @ViewBuilder more: () -> More = { EmptyView() }
    ) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            // Its fix beside the text while the text fits on its lines, else under it.
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .center, spacing: 12) {
                    problemText(urgency: urgency, title: title, detail: detail)
                    Spacer(minLength: 12)
                    fix()
                }
                VStack(alignment: .leading, spacing: 10) {
                    problemText(urgency: urgency, title: title, detail: detail).fixedSize(horizontal: false, vertical: true)
                    fix().padding(.leading, 36)
                }
            }
            more()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Theme.cellPadding)
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(urgency.color.opacity(0.45)))
    }

    private func problemText<Detail: View>(urgency: Urgency, title: String, detail: () -> Detail) -> some View {
        HStack(alignment: .center, spacing: 12) {
            Image(systemName: urgency == .failing ? "xmark.octagon.fill" : "exclamationmark.circle.fill")
                .font(.title3).foregroundStyle(urgency.color)
                .frame(width: 24)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                detail().font(.caption).foregroundStyle(Theme.secondary)
            }
        }
    }

    private var empty: some View {
        Card(padding: 20) {
            VStack(alignment: .leading, spacing: 12) {
                Image(systemName: "person.crop.circle.badge.plus").font(.title2).foregroundStyle(Theme.secondary)
                Text("No accounts yet").font(.headline).foregroundStyle(Theme.text)
                Text("Add a Claude, Codex, Cursor or OpenCode login on one of your machines. herder runs the provider’s own login there, in the account’s own config directory.")
                    .font(.subheadline).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                ActionButton(title: "Add Account", style: .primary) { actions.addAccount() }.frame(maxWidth: 200)
            }
        }
    }

    // MARK: Accounts

    private func accounts(_ rows: [AccountRow], narrow: Bool) -> some View {
        SortableTable("providers.accounts", rows: rows, sortedBy: TableSort(column: "Session left"),
                      columns: columns(narrow: narrow)) { actions.open($0.login) }
    }

    /// Account, machines, the session and weekly windows, and the week's cost; a narrow pane
    /// keeps the account, what its session window leaves and its cost.
    private func columns(narrow: Bool) -> [SortableColumn<AccountRow>] {
        let all = [
            SortableColumn<AccountRow>("Account", value: { "\(ModelCatalog.providerName($0.login.provider)) \($0.login.email ?? $0.login.account.label)" }) { row in
                account(row.login, label: !narrow)
            },
            SortableColumn("Machines", width: ProvidersColumns.machines, value: { $0.login.machines }) { row in
                Text(row.login.machines).foregroundStyle(Theme.secondary)
                    .lineLimit(1).truncationMode(.tail)
                    .help(row.login.on.map(\.name).joined(separator: ", "))
            },
            SortableColumn("Session left", width: narrow ? ProvidersColumns.narrowLimit : ProvidersColumns.wide, value: { Self.left($0.login, \.session) }) { row in
                if row.login.signedOutEverywhere {
                    Text("Signed out").font(Theme.meta.weight(.medium)).foregroundStyle(Theme.accent)
                        .padding(.horizontal, 8).padding(.vertical, 2)
                        .background(Theme.accent.opacity(0.12), in: .capsule)
                } else if row.login.session == nil && row.login.weekly == nil {
                    Text("No limits reported").foregroundStyle(Theme.tertiary)
                } else if let session = row.login.session {
                    limit(session, resets: !narrow)
                } else {
                    Text("")
                }
            },
            SortableColumn("Week left", width: ProvidersColumns.narrow, value: { Self.left($0.login, \.weekly) }) { row in
                // An empty cell would take no room, and shift the cells after it.
                if !row.login.signedOutEverywhere, let weekly = row.login.weekly { limit(weekly, resets: false) } else { Text("") }
            },
            SortableColumn("7 d cost", width: ProvidersColumns.last, alignment: .trailing, value: { $0.usage?.amount.costUsd ?? -1 }) { row in
                Group {
                    if let usage = row.usage, usage.amount.turns > 0 {
                        Text(UsageReport.dollars(usage.amount))
                    } else {
                        Text(report == nil ? "" : "—").foregroundStyle(Theme.tertiary)
                    }
                }
                .monospacedDigit()
            },
        ]
        return narrow ? all.filter { ["Account", "Session left", "7 d cost"].contains($0.title) } : all
    }

    /// What a window leaves, to sort by: a signed-out login first, one that reports none last.
    private static func left(_ login: ProviderAccounts.Login, _ window: KeyPath<ProviderAccounts.Login, WindowLeft?>) -> Double {
        login.signedOutEverywhere ? -1 : login[keyPath: window]?.percentLeft ?? 101
    }

    /// The account's email, or its label without one; `label` adds the label after the email.
    private func account(_ login: ProviderAccounts.Login, label: Bool) -> some View {
        HStack(spacing: 8) {
            ProviderMark(provider: login.provider, size: 14)
            Text(login.email ?? login.account.label).lineLimit(1).truncationMode(.middle)
            if label && login.email != nil {
                Text(login.account.label).foregroundStyle(Theme.tertiary).lineLimit(1)
            }
            if !login.fallbackOn.isEmpty { Badge(text: "Fallback") }
        }
    }

    private func limit(_ window: WindowLeft, resets: Bool = true) -> some View {
        HStack(spacing: 6) {
            UsageBar(percent: window.percentUsed).frame(width: ProvidersColumns.bar)
            Text("\(Int(window.percentLeft.rounded()))%").monospacedDigit()
            if resets && !window.resets.isEmpty {
                Text("· \(window.resets)").foregroundStyle(Theme.tertiary)
            }
        }
        .lineLimit(1)
        .accessibilityElement(children: .combine)
    }
}

/// The column widths the Providers page's tables share, so their columns line up down the page:
/// a flexible first column, then the account table's four.
enum ProvidersColumns {
    static let machines: CGFloat = 110
    static let wide: CGFloat = 190
    static let narrow: CGFloat = 130
    static let last: CGFloat = 72
    /// What a limit's bar takes in a cell, and a narrow table's limit column: bar and percent.
    static let bar: CGFloat = 80
    static let narrowLimit: CGFloat = 124
    /// The tables' gap between columns: the layout grid's.
    static let spacing = Theme.cellPadding
    /// The room the four take after the first column, gaps between them included.
    static let trailing = machines + wide + narrow + last + 3 * spacing
    /// The narrowest a table with all of them takes: below it, a table keeps fewer columns.
    static let wideTable = 200 + spacing + trailing

    /// The width of each of `count` even columns in that room; never under the machines column.
    static func even(_ count: Int) -> CGFloat {
        guard count > 0 else { return trailing }
        return max(machines, (trailing - CGFloat(count - 1) * spacing) / CGFloat(count))
    }
}

/// A section of the Providers page: its heading, a quiet note beside it, its actions on the
/// right, then its content.
struct ProvidersSection<Actions: View, Content: View>: View {
    let title: String
    var count: Int?
    var note: String?
    @ViewBuilder var actions: Actions
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .center, spacing: 8) {
                // The heading keeps its line; the note goes where the actions leave no room for it.
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 8) {
                        SectionHeading(title: title, count: count).fixedSize()
                        if let note { Text(note).font(.caption).foregroundStyle(Theme.tertiary).fixedSize() }
                    }
                    SectionHeading(title: title, count: count).fixedSize()
                }
                Spacer(minLength: 8)
                actions
            }
            .frame(minHeight: Theme.sectionHeight)
            content
        }
    }
}

extension ProvidersSection where Actions == EmptyView {
    init(title: String, count: Int? = nil, note: String? = nil, @ViewBuilder content: () -> Content) {
        self.init(title: title, count: count, note: note, actions: { EmptyView() }, content: content)
    }
}

/// The provider CLIs as a table, a row per provider with the latest version beside its name
/// and an even column per machine, lined up under the accounts table's; where that does not
/// fit, a list per provider. `act` installs or updates one, `retry` runs a failed install again.
struct VersionMatrix: View {
    let overview: ProvidersOverview
    let act: (ProviderVersions.Cell) -> Void
    let retry: (JobKey) -> Void
    let fix: (JobKey) -> Void

    var body: some View {
        let machines = overview.versions.rows.first?.cells ?? []
        ViewThatFits(in: .horizontal) {
            SortableTable("providers.versions", rows: overview.versions.rows, sortedBy: TableSort(column: "CLI"),
                          columns: columns(machines))
                .frame(minWidth: 160 + CGFloat(machines.count) * (ProvidersColumns.even(machines.count) + ProvidersColumns.spacing))
            list
        }
    }

    private func columns(_ machines: [ProviderVersions.Cell]) -> [SortableColumn<ProviderVersions.Row>] {
        let width = ProvidersColumns.even(machines.count)
        return [SortableColumn("CLI") { provider($0) }]
            + machines.indices.map { index in
                SortableColumn<ProviderVersions.Row>(machines[index].machine, width: width) { row in
                    if row.cells.indices.contains(index) {
                        let cell = row.cells[index]
                        MatrixCell(cell: cell, job: overview.job(cell), act: act, retry: retry, fix: fixer(cell))
                    }
                }
            }
    }

    private var list: some View {
        VStack(spacing: 0) {
            ForEach(overview.versions.rows) { row in
                VStack(alignment: .leading, spacing: 8) {
                    provider(row)
                    ForEach(row.cells) { cell in
                        HStack {
                            Text(cell.machine).font(.caption).foregroundStyle(Theme.secondary)
                            Spacer()
                            MatrixCell(cell: cell, job: overview.job(cell), act: act, retry: retry, fix: fixer(cell))
                        }
                    }
                }
                .padding(.vertical, 12)
                .overlay(alignment: .top) { Rectangle().fill(Theme.stroke).frame(height: 1) }
            }
        }
    }

    /// Fix with AI for the cell's failed install, where it is offered.
    private func fixer(_ cell: ProviderVersions.Cell) -> ((JobKey) -> Void)? {
        overview.fixable.contains(JobKey(hostId: cell.hostId, kind: .install(cell.provider))) ? fix : nil
    }

    private func provider(_ row: ProviderVersions.Row) -> some View {
        HStack(spacing: 8) {
            ProviderMark(provider: row.provider, size: 14)
            Text(ModelCatalog.providerName(row.provider)).font(Theme.body).foregroundStyle(Theme.text)
            if let latest = row.latest { Text(latest).font(Theme.monoSmall).foregroundStyle(Theme.tertiary) }
        }
        .lineLimit(1)
    }
}

/// One CLI on one machine in the matrix: a quiet ✓ when current; when outdated its version and
/// Update; when missing a dash and Install; while this device installs it, the job's progress.
struct MatrixCell: View {
    let cell: ProviderVersions.Cell
    let job: JobProgress?
    let act: (ProviderVersions.Cell) -> Void
    let retry: (JobKey) -> Void
    /// Fix with AI for a failed install; nil where it is not offered.
    var fix: ((JobKey) -> Void)?

    var body: some View {
        HStack(spacing: 6) {
            switch job {
            case .queued?:
                chip("Queued", Urgency.quiet.color)
            case .starting?, .installing?:
                Image(systemName: "arrow.triangle.2.circlepath").font(.caption.weight(.semibold))
                    .foregroundStyle(Urgency.running.color)
                    .symbolEffect(.rotate, options: .repeating)
                Text(cell.state == .missing ? "Installing" : "Updating").font(.caption.weight(.medium))
                    .foregroundStyle(Urgency.running.color)
            case .failed(let message)?:
                let key = JobKey(hostId: cell.hostId, kind: .install(cell.provider))
                Button { retry(key) } label: {
                    chip("Failed · Retry", Urgency.failing.color)
                }
                .buttonStyle(.plain)
                .help(message)
                if let fix {
                    Button { fix(key) } label: {
                        Image(systemName: "sparkles").font(.caption.weight(.semibold)).foregroundStyle(Theme.text)
                            .frame(width: 22, height: 22).contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .help("Fix with AI")
                    .accessibilityLabel("Fix with AI")
                }
            case .installed?, .updated?, .upToDate?:
                check
                Text("just now").font(.caption).foregroundStyle(Theme.tertiary)
            default:
                version
            }
        }
        .lineLimit(1)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder private var version: some View {
        switch cell.state {
        case .current:
            check
        case .unknown:
            Text(cell.version ?? "Installed").font(Theme.monoSmall).foregroundStyle(Theme.secondary)
        case .outdated:
            Text(cell.version ?? "").font(Theme.monoSmall).foregroundStyle(Theme.text)
            action
        case .missing:
            Text("—").foregroundStyle(Theme.tertiary)
            action
        }
    }

    private var check: some View {
        Image(systemName: "checkmark").font(.caption.weight(.semibold)).foregroundStyle(Urgency.done.color)
            .accessibilityLabel(cell.version.map { "Current, \($0)" } ?? "Current")
    }

    @ViewBuilder private var action: some View {
        if cell.action != nil { CellAction(cell: cell, act: act) }
    }

    private func chip(_ text: String, _ tint: Color) -> some View {
        Text(text).font(.caption.weight(.medium)).foregroundStyle(tint)
            .padding(.horizontal, 7).padding(.vertical, 2)
            .background(tint.opacity(0.12), in: .capsule)
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
                    .padding(.horizontal, Theme.cellPadding)
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
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 8)
        .frame(minHeight: Theme.rowHeight)
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
            if cell.action != nil { CellAction(cell: cell, act: act) }
        }
        .lineLimit(1)
        .accessibilityElement(children: .combine)
    }
}

/// A CLI's Install or Update, as a small button; dimmed, with why on hover, only when it
/// cannot run.
struct CellAction: View {
    let cell: ProviderVersions.Cell
    let act: (ProviderVersions.Cell) -> Void

    var body: some View {
        if let action = cell.action {
            PillButton(title: action.title) { act(cell) }
                .disabled(cell.blocked != nil)
                .help(cell.blocked ?? "\(action.title) \(ModelCatalog.providerName(cell.provider)) on \(cell.machine)")
        }
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
    /// Green with room to spare, the limit colour getting low (amber means needs you),
    /// vermilion nearly out; the light values are darker so they hold up on grey paper.
    var color: Color {
        switch self {
        case .plenty: Color(light: 0x2E7D5B, dark: 0x009E73)
        case .low: Theme.limit
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

/// A failed job's last lines of output, as its card's details show them.
struct FailureOutput: View {
    let text: String

    var body: some View {
        ScrollView {
            Text(text).font(Theme.monoSmall).foregroundStyle(Theme.secondary).textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
        }
        .defaultScrollAnchor(.bottom)
        .frame(maxHeight: 220)
        .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
    }
}
