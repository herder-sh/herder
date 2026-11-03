import Herder
import SwiftUI

/// One row of an account page's Machines table: an account of the login on a machine, or a
/// machine that lacks the login; how the login stands there, and how many sessions use it.
struct AccountMachine: Equatable, Identifiable {
    enum LoginState: Equatable {
        case signedIn
        /// The provider reported the account logged out, at `since` when known.
        case signedOut(since: Date?)
        case notSetUp
    }

    let place: ProviderAccounts.Place
    /// The login's account there; none where it is not set up.
    let account: Account?
    let state: LoginState
    let sessions: Int

    var id: String { "\(place.hostId)/\(account?.accountId ?? "")" }

    /// Runs the provider's own login again in the account's config directory, on its machine,
    /// as a job in Activity; nothing where the login is not set up.
    @MainActor @discardableResult
    func signIn(in fleet: Fleet) -> JobKey? {
        account.map { fleet.relogin($0.accountId, on: place.hostId) }
    }

    /// The machines that have the login, an account a row, then those that lack it.
    static func rows(_ login: ProviderAccounts.Login, machines: [Machine],
                     summaries: [MachineSummary]) -> [AccountMachine] {
        let on = login.on.flatMap { place in
            let accounts = machines.first { $0.hostId == place.hostId }?.accounts
                .filter { place.accounts.contains($0.accountId) } ?? []
            let counts = summaries.first { $0.hostId == place.hostId }?.accounts ?? []
            return accounts.map { account in
                AccountMachine(place: place, account: account,
                               state: account.needsLogin.map { .signedOut(since: Timestamp.date($0)) } ?? .signedIn,
                               sessions: counts.first { $0.accountId == account.accountId }?.sessions ?? 0)
            }
        }
        return on + login.missing.map { AccountMachine(place: $0, account: nil, state: .notSetUp, sessions: 0) }
    }
}

extension Account {
    /// The command that sets this account's `label` and `fallback` (nil keeps it). A login
    /// account takes `configDir`, nil for the provider's default; an endpoint account never
    /// takes one, as the daemon refuses it there, and takes `endpoint` and `key` or `keySecret`
    /// instead, each nil to keep it. The key is sent only when replaced, and never read back.
    func settings(label: String, configDir: String?, fallback: Bool?, endpoint: String? = nil,
                  key: String? = nil, keySecret: SecretRef? = nil) -> CommandBody {
        let isEndpoint = self.endpoint != nil
        return .setAccountSettings(accountId: accountId, label: label, configDir: isEndpoint ? nil : configDir,
                                   fallback: fallback, endpoint: isEndpoint ? endpoint : nil,
                                   key: isEndpoint ? key : nil, keySecret: isEndpoint ? keySecret : nil)
    }
}

extension WindowLeft {
    /// When the window resets, by the clock and from now: "resets 23:20 · in 2 h 10 min", with
    /// the day when it is not today: "resets Thu 09:00 · in 2 d 14 h".
    static func resetLine(_ at: Date?, now: Date, calendar: Calendar = .current) -> String {
        guard let at else { return "" }
        let time = Date.FormatStyle(date: .omitted, time: .shortened, locale: calendar.locale ?? .current,
                                    calendar: calendar, timeZone: calendar.timeZone)
        let clock = calendar.isDate(at, inSameDayAs: now)
            ? at.formatted(time)
            : "\(at.formatted(Date.FormatStyle(locale: calendar.locale ?? .current, calendar: calendar, timeZone: calendar.timeZone).weekday(.abbreviated))) \(at.formatted(time))"
        let minutes = max(0, Int(at.timeIntervalSince(now)) / 60)
        let (days, hours) = (minutes / 1440, minutes % 1440 / 60)
        let left = if days > 0 { "\(days) d \(hours) h" }
            else if hours > 0 { "\(hours) h \(minutes % 60) min" }
            else if minutes > 0 { "\(minutes) min" }
            else { "under a minute" }
        return "resets \(clock) · in \(left)"
    }
}

/// One login's page: what is left of its plan's limits, in large type, as that is why it is
/// opened; what it cost over the last week; each machine with how the login stands there and
/// its fix, Sign in again or Set up; then its display label and its role in rotation, which
/// apply on every machine the user owns. An endpoint account shows its endpoint in place of
/// limits and config directories, has no login to sign in to again, and lets an owner change
/// the endpoint and replace the key, which is never shown.
struct AccountPage: View {
    let fleet: Fleet
    let loginId: String
    /// The id the login has since its endpoint changed here, as accounts on one endpoint are
    /// one login.
    @State private var movedTo: String?
    @State private var endpoint = ""
    @State private var key = ""
    /// Whether the owner chose to type a key in place of the vault's.
    @State private var typingKey = false
    @FocusState private var editingEndpoint: Bool
    @State private var setup: ProvidersView.Setup?
    @State private var label = ""
    @State private var fallback = false
    @State private var editingDirectory: AccountMachine?
    @State private var directory = ""
    @State private var error: String?
    @State private var saves = Autosave()
    @FocusState private var editingLabel: Bool
    private let period = UsagePeriod.week

    private var login: ProviderAccounts.Login? {
        ProviderAccounts(machines: fleet.machines).groups.flatMap(\.logins).first { $0.id == (movedTo ?? loginId) }
    }

    var body: some View {
        let login = login
        let rows = login.map { AccountMachine.rows($0, machines: fleet.machines, summaries: fleet.lists.machines) } ?? []
        DetailPage(title: login.map { $0.email ?? $0.account.label } ?? "Account",
                   chip: rows.contains { if case .signedOut = $0.state { true } else { false } }
                       ? StatusChip(text: "Signed out", urgency: .needsYou) : nil,
                   status: login.map(status) ?? "") {
            if let login { ProviderMark(provider: login.provider, size: 28) }
        } actions: {
            if let login, let place = login.missing.first(where: { $0.owner && $0.connected }) {
                PaneButton(title: "Set up on \(place.name)", symbol: "plus") { setUp(login, on: place) }
            }
        } content: {
            if let login, let url = login.endpoint {
                endpointSection(url, fromVault: login.keyFromVault).detailSection("Endpoint")
                usage(login).detailSection(period.title)
                machines(login, rows: rows).detailSection("Machines")
                settings(login, rows: rows).detailSection("Settings")
                AboutSection {
                    DetailRow(label: "Provider", value: ModelCatalog.providerName(login.provider))
                    DetailRow(label: "Endpoint", value: url, mono: true)
                    ForEach(rows.filter { $0.account != nil }) { row in
                        DetailRow(label: row.place.name, value: row.account?.accountId ?? "", mono: true)
                    }
                }
            } else if let login {
                limits(login).detailSection("Limits")
                usage(login).detailSection(period.title)
                machines(login, rows: rows).detailSection("Machines")
                settings(login, rows: rows).detailSection("Settings")
                AboutSection {
                    DetailRow(label: "Provider", value: ModelCatalog.providerName(login.provider))
                    if let email = login.email { DetailRow(label: "Email", value: email) }
                    ForEach(rows.filter { $0.account != nil }) { row in
                        DetailRow(label: row.place.name, value: row.account?.accountId ?? "", mono: true)
                    }
                }
            } else {
                Text("This account is no longer on any machine.").foregroundStyle(Theme.secondary)
            }
        }
        .task { await fleet.refreshUsage(over: period) }
        .task(id: login?.endpoint) {
            // Whether each machine reaches the vault's logins at this endpoint, to offer its key.
            if login?.endpoint != nil { await fleet.refreshVaultLogins(on: editable(rows).map(\.0)) }
        }
        .onAppear { reset() }
        .onChange(of: loginId) { movedTo = nil; reset() }
        .onChange(of: editingEndpoint) { if !editingEndpoint { saveEndpoint(rows) } }
        .onChange(of: editingLabel) { if !editingLabel { saveLabel(rows) } }
        .sheet(item: $setup) { setup in
            AddAccountSheet(fleet: fleet, hostId: setup.hostId, initialProvider: setup.draft.provider,
                            initialDraft: setup.draft)
        }
        .alert("Config directory on \(editingDirectory?.place.name ?? "")", isPresented: Binding(
            get: { editingDirectory != nil }, set: { if !$0 { editingDirectory = nil } })) {
            TextField("Provider default", text: $directory)
                .autocorrectionDisabled()
            Button("Save") { if let editingDirectory { saveDirectory(editingDirectory) } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Leave it empty for the provider’s default login. Archive every session on the machine first.")
        }
    }

    /// The fields as the login has them.
    private func reset() {
        guard let login else { return }
        label = login.account.label
        fallback = !login.fallbackOn.isEmpty && login.fallbackOn.count == login.on.count
        endpoint = login.endpoint ?? ""
        key = ""
        typingKey = false
    }

    /// "Claude · work · on 2 machines · primary": accounts with the same provider and email
    /// share one login, and its limits.
    private func status(_ login: ProviderAccounts.Login) -> String {
        let on = login.on.count == 1 ? "on 1 machine" : "on \(login.on.count) machines"
        let rotation = login.fallbackOn.isEmpty ? "primary"
            : login.fallbackOn.count == login.on.count ? "fallback"
            : "fallback on \(login.fallbackOn.map(\.name).joined(separator: ", "))"
        let parts: [String?] = [ModelCatalog.providerName(login.provider), login.email == nil ? nil : login.account.label,
                                login.endpoint == nil ? nil : "endpoint", on, rotation]
        return parts.compactMap(\.self).joined(separator: " · ")
    }

    // MARK: Endpoint

    /// Where an endpoint account runs, in place of limits: the server behind it holds them.
    private func endpointSection(_ url: String, fromVault: Bool) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(url).font(Theme.mono).foregroundStyle(Theme.text).lineLimit(1).truncationMode(.middle)
                .textSelection(.enabled)
            Text(ProviderAccounts.Login.endpointNote(fromVault: fromVault))
                .font(.caption).foregroundStyle(Theme.tertiary)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
        .accessibilityElement(children: .combine)
    }

    // MARK: Limits

    @ViewBuilder private func limits(_ login: ProviderAccounts.Login) -> some View {
        if login.session == nil && login.weekly == nil {
            Text("No limits reported yet").font(.subheadline).foregroundStyle(Theme.tertiary)
        } else {
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .top, spacing: 10) { windows(login) }
                VStack(spacing: 10) { windows(login) }
            }
        }
    }

    @ViewBuilder private func windows(_ login: ProviderAccounts.Login) -> some View {
        if let session = login.session { window("Session window", session) }
        if let weekly = login.weekly { window("Weekly window", weekly) }
    }

    private func window(_ label: String, _ window: WindowLeft) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(label).font(.caption).foregroundStyle(Theme.secondary)
            Text("\(Int(window.percentLeft.rounded()))% left")
                .font(.system(size: 26, weight: .semibold)).monospacedDigit()
                .foregroundStyle(window.headroom == .plenty ? Theme.text : window.headroom.color)
            HeadroomMeter(window: window)
            Text(WindowLeft.resetLine(window.resetsAt, now: .now)).font(.caption).foregroundStyle(Theme.tertiary)
        }
        .lineLimit(1)
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .frame(minWidth: 240, maxWidth: .infinity, alignment: .leading)
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
        .accessibilityElement(children: .combine)
    }

    // MARK: Usage

    /// The week's cost and tokens on every machine, and how often the login ran out.
    private func usage(_ login: ProviderAccounts.Login) -> some View {
        let report = fleet.usageCache[period].map { UsageReport($0.machines) }
        let usage = report?.logins[login.id] ?? UsageReport.Login()
        return VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: period.title)
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                if report == nil {
                    ProgressView().controlSize(.small)
                } else {
                    Text(UsageReport.dollars(usage.amount)).font(.title3.weight(.semibold)).foregroundStyle(Theme.text)
                    Text("\(UsageReport.tokens(usage.amount.tokens)) tokens at API prices")
                        .font(.subheadline).foregroundStyle(Theme.secondary)
                    Spacer(minLength: 12)
                    Text(outages(usage)).font(.caption).foregroundStyle(Theme.tertiary)
                }
            }
            .monospacedDigit()
            .lineLimit(1)
        }
    }

    /// "2 limit hits · 1 failover out · 0 in".
    private func outages(_ usage: UsageReport.Login) -> String {
        let hits = usage.limitHits == 1 ? "1 limit hit" : "\(usage.limitHits) limit hits"
        let out = usage.failoversOut == 1 ? "1 failover out" : "\(usage.failoversOut) failovers out"
        return "\(hits) · \(out) · \(usage.failoversIn) in"
    }

    // MARK: Machines

    private func machines(_ login: ProviderAccounts.Login, rows: [AccountMachine]) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: "Machines", count: rows.count)
            ViewThatFits(in: .horizontal) {
                table(login, rows: rows).frame(minWidth: 600, maxWidth: .infinity, alignment: .leading)
                list(login, rows: rows)
            }
            .background(.card, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
            if let error { Text(error).font(.footnote).foregroundStyle(Theme.failure) }
        }
    }

    private func table(_ login: ProviderAccounts.Login, rows: [AccountMachine]) -> some View {
        Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 0) {
            GridRow {
                Text("Machine")
                Text(login.endpoint == nil ? "Login" : "State")
                Text(login.endpoint == nil ? "Config directory" : "Account")
                Text("Sessions").gridColumnAlignment(.trailing)
                Text("")
            }
            .font(.caption.weight(.semibold))
            .foregroundStyle(Theme.tertiary)
            .padding(.vertical, 10)
            ForEach(rows) { row in
                Divider().gridCellUnsizedAxes(.horizontal)
                GridRow {
                    Text(row.place.name).font(.subheadline).foregroundStyle(Theme.text)
                    state(row)
                    Text(directory(row)).font(Theme.monoSmall).foregroundStyle(Theme.secondary)
                        .truncationMode(.middle)
                    Text(row.account == nil ? "" : "\(row.sessions)").font(.subheadline.monospacedDigit())
                        .foregroundStyle(Theme.text)
                    fix(login, row: row).gridColumnAlignment(.trailing)
                }
                .lineLimit(1)
                .frame(minHeight: 44)
            }
        }
        .padding(.horizontal, 14)
    }

    private func list(_ login: ProviderAccounts.Login, rows: [AccountMachine]) -> some View {
        VStack(spacing: 0) {
            ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                if index > 0 { RowDivider() }
                HStack(spacing: 10) {
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: 8) {
                            Text(row.place.name).font(.subheadline).foregroundStyle(Theme.text)
                            state(row)
                        }
                        if row.account != nil {
                            Text("\(directory(row)) · \(row.sessions == 1 ? "1 session" : "\(row.sessions) sessions")")
                                .font(Theme.monoSmall).foregroundStyle(Theme.tertiary).truncationMode(.middle)
                        }
                    }
                    .lineLimit(1)
                    Spacer(minLength: 8)
                    fix(login, row: row)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
                .frame(minHeight: 52)
            }
        }
    }

    @ViewBuilder private func state(_ row: AccountMachine) -> some View {
        switch row.state {
        case .signedIn:
            let ready = row.account?.endpoint == nil ? "Signed in" : "Ready"
            StatusChip(text: row.place.connected ? ready : "Not connected",
                       urgency: row.place.connected ? .done : .quiet)
        case .signedOut(let since):
            let age = Timestamp.age(since, now: .now)
            StatusChip(text: age.isEmpty ? "Signed out" : "Signed out · \(age == "now" ? "just now" : "\(age) ago")",
                       urgency: .needsYou)
        case .notSetUp:
            Text("Not set up").font(.caption).foregroundStyle(Theme.tertiary)
        }
    }

    /// The account's config directory there; an endpoint account's id, as herder picks its
    /// directory and it needs no login.
    private func directory(_ row: AccountMachine) -> String {
        guard let account = row.account else { return "" }
        if account.endpoint != nil { return account.accountId }
        return account.configDir ?? "default"
    }

    /// The row's fix, Sign in again or Set up; for a signed-in account, the rest behind ⋯.
    @ViewBuilder private func fix(_ login: ProviderAccounts.Login, row: AccountMachine) -> some View {
        let canManage = row.place.owner && row.place.connected
        switch row.state {
        case .notSetUp:
            PillButton(title: row.place.needsInstall ? "Install & Set Up" : "Set up") { setUp(login, on: row.place) }
                .disabled(!canManage)
        case .signedOut:
            HStack(spacing: 6) {
                PillButton(title: "Sign in again", prominent: true) { row.signIn(in: fleet) }
                    .disabled(!canManage)
                more(row)
            }
        case .signedIn:
            // An endpoint account has no login to sign in to, nor a directory to choose.
            if row.account?.endpoint == nil { more(row) }
        }
    }

    private func more(_ row: AccountMachine) -> some View {
        Menu {
            Button("Sign in again", systemImage: "person.crop.circle.badge.checkmark") {
                row.signIn(in: fleet)
            }
            Button("Change config directory…", systemImage: "folder") {
                directory = row.account?.configDir ?? ""
                editingDirectory = row
            }
        } label: {
            Image(systemName: "ellipsis").font(.subheadline.weight(.semibold)).foregroundStyle(Theme.secondary)
                .frame(width: 28, height: 28).contentShape(.rect)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(!(row.place.owner && row.place.connected))
        .accessibilityLabel("More for \(row.place.name)")
    }

    // MARK: Settings

    private func settings(_ login: ProviderAccounts.Login, rows: [AccountMachine]) -> some View {
        let canManage = rows.contains { $0.account != nil && $0.place.owner && $0.place.connected }
        return SettingsList(title: "Settings") {
            SettingRow(label: "Display label", hint: "On every machine you own", saved: saves["Display label"]) {
                TextField(login.account.accountId, text: $label)
                    .textFieldStyle(.plain)
                    .foregroundStyle(Theme.text)
                    .multilineTextAlignment(.trailing)
                    .autocorrectionDisabled()
                    .focused($editingLabel)
                    .onSubmit { saveLabel(rows) }
                    .padding(.horizontal, 10)
                    .frame(maxWidth: 240, minHeight: 32)
                    .background(.card, in: .rect(cornerRadius: 7))
                    .accessibilityIdentifier("account-label")
            }
            // The endpoint and its key are the owner's: a member never sees the key's field.
            if login.endpoint != nil && rows.contains(where: { $0.place.owner }) {
                RowDivider()
                SettingRow(label: "Endpoint", hint: "On every machine you own", saved: saves["Endpoint"]) {
                    TextField("http://127.0.0.1:8317", text: $endpoint)
                        .textFieldStyle(.plain)
                        .font(Theme.monoSmall)
                        .foregroundStyle(Theme.text)
                        .multilineTextAlignment(.trailing)
                        .autocorrectionDisabled()
                        .focused($editingEndpoint)
                        .onSubmit { saveEndpoint(rows) }
                        .padding(.horizontal, 10)
                        .frame(maxWidth: 240, minHeight: 32)
                        .background(.card, in: .rect(cornerRadius: 7))
                        .accessibilityIdentifier("account-endpoint")
                }
                RowDivider()
                SettingRow(label: "Key", hint: Self.keyHint(fromVault: login.keyFromVault, typing: typingKey),
                           saved: saves["Key"]) {
                    HStack(spacing: 8) {
                        if login.keyFromVault && !typingKey {
                            Text("Key from the vault").foregroundStyle(Theme.secondary)
                                .accessibilityIdentifier("account-key-vault")
                            PillButton(title: "Use own key") { typingKey = true }
                        } else {
                            SecureField(login.keyFromVault ? "Type a key" : "Unchanged", text: $key)
                                .textFieldStyle(.plain)
                                .foregroundStyle(Theme.text)
                                .multilineTextAlignment(.trailing)
                                .onSubmit { saveKey(rows) }
                                .padding(.horizontal, 10)
                                .frame(maxWidth: 240, minHeight: 32)
                                .background(.card, in: .rect(cornerRadius: 7))
                                .accessibilityIdentifier("account-key")
                            if login.keyFromVault {
                                PillButton(title: "Cancel") { typingKey = false; key = "" }
                            } else if canUseVaultKey(login, rows: rows) {
                                PillButton(title: "Use the vault’s") { useVaultKey(rows) }
                            }
                        }
                    }
                }
            }
            RowDivider()
            SettingRow(label: "Rotation", hint: rotationHint(login),
                       help: "A fallback login is picked for new sessions and failover only once every other \(ModelCatalog.providerName(login.provider)) account is at its limit or signed out. Choosing it by name still uses it.",
                       saved: saves["Rotation"]) {
                Picker("Rotation", selection: $fallback) {
                    Text("Primary").tag(false)
                    Text("Fallback").tag(true)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 180)
                .onChange(of: fallback) { saveRotation(rows) }
            }
        }
        .font(.subheadline)
        .disabled(!canManage)
    }

    static func keyHint(fromVault: Bool, typing: Bool) -> String {
        if fromVault && !typing { return "Each machine’s own, fetched from its vault" }
        return fromVault ? "Kept on each machine in place of the vault’s" : "Never shown; type a new one to replace it"
    }

    /// Whether every machine the owner can change reaches its vault's logins at the login's
    /// endpoint, so the vault's key works there.
    private func canUseVaultKey(_ login: ProviderAccounts.Login, rows: [AccountMachine]) -> Bool {
        let editable = editable(rows)
        return !editable.isEmpty && editable.allSatisfy { fleet.vaultLogins(on: $0.0) == login.endpoint }
    }

    private func rotationHint(_ login: ProviderAccounts.Login) -> String {
        let fallbackOn = login.fallbackOn
        if !fallbackOn.isEmpty && fallbackOn.count < login.on.count {
            return "Fallback on \(fallbackOn.map(\.name).joined(separator: ", ")) only"
        }
        return "Fallback is used only once every primary is out"
    }

    // MARK: Actions

    private func setUp(_ login: ProviderAccounts.Login, on place: ProviderAccounts.Place) {
        let taken = AccountDraft.taken(on: fleet.machines.first { $0.hostId == place.hostId },
                                       pending: fleet.activity.pendingIds(on: place.hostId))
        if place.needsInstall { fleet.install(login.provider, on: place.hostId) }
        setup = ProvidersView.Setup(hostId: place.hostId, draft: login.draft(taken: taken))
    }

    /// The accounts a change applies to: the login's, on every connected machine the user owns.
    private func editable(_ rows: [AccountMachine]) -> [(HostId, Account)] {
        rows.compactMap { row in
            guard let account = row.account, row.place.owner, row.place.connected else { return nil }
            return (row.place.hostId, account)
        }
    }

    private func saveLabel(_ rows: [AccountMachine]) {
        let label = label.trimmingCharacters(in: .whitespacesAndNewlines)
        let changed = editable(rows).filter { $0.1.label != label }
        guard !label.isEmpty, !changed.isEmpty else { return }
        Task {
            await saves.save("Display label") {
                for (hostId, account) in changed {
                    try await send(account, on: hostId, label: label, configDir: account.configDir, fallback: nil)
                }
            }
        }
    }

    private func saveRotation(_ rows: [AccountMachine]) {
        let chosen = fallback
        let changed = editable(rows).filter { $0.1.fallback != chosen }
        guard !changed.isEmpty else { return }
        Task {
            await saves.save("Rotation") {
                for (hostId, account) in changed {
                    try await send(account, on: hostId, label: account.label, configDir: account.configDir,
                                   fallback: chosen)
                }
            }
        }
    }

    private func saveEndpoint(_ rows: [AccountMachine]) {
        let url = endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        let changed = editable(rows).filter { $0.1.endpoint != nil && $0.1.endpoint != url }
        guard !changed.isEmpty else { return }
        guard AccountDraft.validEndpoint(url) else {
            saves.fail("Endpoint", "Use an http:// or https:// URL.")
            return
        }
        Task {
            await saves.save("Endpoint") {
                for (hostId, account) in changed {
                    _ = try await fleet.client.send(hostId: hostId, command: account.settings(
                        label: account.label, configDir: nil, fallback: nil, endpoint: url))
                }
                // Accounts on one endpoint are one login: follow it to its new id.
                if let provider = changed.first?.1.provider {
                    movedTo = ProviderAccounts.endpointKey(provider: provider, endpoint: url)
                }
            }
        }
    }

    private func saveKey(_ rows: [AccountMachine]) {
        let typed = key.trimmingCharacters(in: .whitespacesAndNewlines)
        let changed = editable(rows).filter { $0.1.endpoint != nil }
        guard !typed.isEmpty, !changed.isEmpty else { return }
        // Not kept once sent: the field goes back to "Unchanged".
        key = ""
        typingKey = false
        Task {
            await saves.save("Key") {
                for (hostId, account) in changed {
                    _ = try await fleet.client.send(hostId: hostId, command: account.settings(
                        label: account.label, configDir: nil, fallback: nil, key: typed))
                }
            }
        }
    }

    /// Has the login's accounts fetch each machine's key from its vault, in place of the key
    /// they hold.
    private func useVaultKey(_ rows: [AccountMachine]) {
        let changed = editable(rows).filter { $0.1.endpoint != nil && $0.1.keySecret == nil }
        guard !changed.isEmpty else { return }
        key = ""
        Task {
            await saves.save("Key") {
                for (hostId, account) in changed {
                    _ = try await fleet.client.send(hostId: hostId, command: account.settings(
                        label: account.label, configDir: nil, fallback: nil, keySecret: AccountDraft.vaultKeySecret))
                }
            }
        }
    }

    private func saveDirectory(_ row: AccountMachine) {
        guard let account = row.account else { return }
        let path = directory.trimmingCharacters(in: .whitespacesAndNewlines)
        guard path.isEmpty || AccountDraft.validPath(path) else {
            error = "Use an absolute path or a path starting with ~/."
            return
        }
        Task {
            do {
                try await send(account, on: row.place.hostId, label: account.label,
                               configDir: path.isEmpty ? nil : path, fallback: nil)
                error = nil
            } catch {
                self.error = "\(row.place.name): \(describe(error))"
            }
        }
    }

    /// Sets the account's settings on its machine; a nil `configDir` is the provider's default.
    private func send(_ account: Account, on hostId: HostId, label: String, configDir: String?,
                      fallback: Bool?) async throws {
        _ = try await fleet.client.send(hostId: hostId, command: account.settings(
            label: label, configDir: configDir, fallback: fallback))
    }
}
