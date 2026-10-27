import Herder
import SwiftUI

/// What is wrong on a machine that its page can fix: the accounts signed out there, each with
/// the sessions it holds up, and the provider CLIs herder can update there now.
struct MachineAttention: Equatable {
    struct SignedOut: Equatable, Identifiable {
        let account: Account
        /// Its sessions there waiting for it to sign in again.
        let waiting: Int
        let since: Date?

        var id: AccountId { account.accountId }
    }

    let signedOut: [SignedOut]
    let updates: [ProviderVersions.Cell]

    init(machine: Machine, versions: ProviderVersions) {
        signedOut = machine.accounts.compactMap { account in
            guard let since = account.needsLogin else { return nil }
            let waiting = machine.sessions.filter { $0.accountId == account.accountId && $0.status == .needsLogin }.count
            return SignedOut(account: account, waiting: waiting, since: Timestamp.date(since))
        }
        updates = versions.updatable.filter { $0.hostId == machine.hostId }
    }

    var isEmpty: Bool { signedOut.isEmpty && updates.isEmpty }

    /// "2 CLI updates".
    var updatesTitle: String { updates.count == 1 ? "1 CLI update" : "\(updates.count) CLI updates" }

    /// "Claude 2.1.9 → 2.1.20 · Codex 0.44.0 → 0.46.0".
    var updatesDetail: String {
        updates.map { cell in
            [ModelCatalog.providerName(cell.provider), cell.version, cell.available.map { "→ \($0)" }]
                .compactMap(\.self).joined(separator: " ")
        }
        .joined(separator: " · ")
    }

    /// "Update", "Update both", "Update all".
    var updateTitle: String {
        switch updates.count {
        case 1: "Update"
        case 2: "Update both"
        default: "Update all"
        }
    }

    /// "2 sessions waiting · since 14m ago".
    static func detail(_ signedOut: SignedOut, now: Date) -> String {
        let waiting: String? = switch signedOut.waiting {
        case 0: nil
        case 1: "1 session waiting"
        default: "\(signedOut.waiting) sessions waiting"
        }
        let since = signedOut.since.map { "since \(Timestamp.age($0, now: now)) ago" }
        return [waiting, since].compactMap(\.self).joined(separator: " · ")
    }
}

/// A machine's page: what needs fixing there first, then its accounts, provider CLIs, MCP
/// servers, turn limit, backup, secrets, people and connection, its identifiers, and forgetting it. Each section
/// is its own view below, listed once in `body`.
struct MachinePage: View {
    let fleet: Fleet
    let hostId: HostId
    @Binding var sheet: AppSheet?
    @Environment(\.pages) private var pages
    @State private var name = ""
    @State private var newAddress = ""
    @State private var addingAccount = false
    @State private var setup: ProvidersView.Setup?
    @State private var confirmingForget = false
    @State private var showingAddresses = false
    @State private var error: String?
    @State private var saves = Autosave()
    @FocusState private var editingName: Bool
    /// The address a Connect is switching to, until it answers or fails.
    @State private var connecting: String?
    /// The address the last Connect failed on, and why.
    @State private var connectFailure: AddressStatus.Failure?

    var body: some View {
        let machine = fleet.machines.first { $0.hostId == hostId }
        DetailPage(title: machine?.name ?? "Machine", chip: machine.map(Self.chip), status: machine.map(status) ?? "") {
            if let machine {
                let runsSessions = machine.hosts.isEmpty
                if runsSessions { attention(machine) }
                if runsSessions {
                    accounts(machine).detailSection("Accounts")
                    clis(machine)
                    McpMachineSettings(fleet: fleet, machine: machine).detailSection("MCP servers")
                }
                if runsSessions, let resources = machine.resources {
                    capacity(machine, resources).detailSection("Capacity")
                }
                if runsSessions && machine.role == .owner && machine.connection == .connected {
                    BackupSection(fleet: fleet, machine: machine, sheet: $sheet).detailSection("Backup")
                    SecretsSection(fleet: fleet, machine: machine, sheet: $sheet).detailSection("Secrets")
                    MachineBotsSection(fleet: fleet, machine: machine).detailSection("Bots")
                } else if runsSessions && machine.role == .member {
                    SecretsSection(fleet: fleet, machine: machine, sheet: $sheet).detailSection("Secrets")
                }
                if machine.connection == .connected {
                    PeopleSection(fleet: fleet, machine: machine).id(machine.hostId).detailSection("People")
                }
                connection(machine).detailSection("Connection")
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Theme.failure)
                }
                if runsSessions && machine.role == .owner {
                    herder(machine).detailSection("herder")
                }
                AboutSection {
                    DetailRow(label: "Fingerprint", value: grouped(machine.fingerprint), mono: true)
                    DetailRow(label: "Host id", value: machine.hostId, mono: true)
                    if !runsSessions {
                        DetailRow(label: "Vault of", value: machine.hosts.map(\.hostName).joined(separator: ", "))
                    }
                }
                DangerZone {
                    SettingRow(label: "Forget this machine", hint: "Pair again to get it back") {
                        DangerButton(title: "Forget…") { confirmingForget = true }
                    }
                }
            } else {
                Text("This machine is no longer paired with this device.").foregroundStyle(Theme.secondary)
            }
        }
        .sheet(isPresented: $addingAccount) { AddAccountSheet(fleet: fleet, hostId: hostId) }
        .sheet(item: $setup) { setup in
            AddAccountSheet(fleet: fleet, hostId: hostId, initialProvider: setup.draft.provider, initialDraft: setup.draft)
        }
        .onAppear { name = machine?.name ?? "" }
        .onChange(of: editingName) { if !editingName { rename() } }
        .confirmationDialog("Forget \(machine?.name ?? "") on this device?", isPresented: $confirmingForget,
                            titleVisibility: .visible) {
            Button("Forget", role: .destructive) {
                perform { try fleet.forget(hostId) }
                pages.close()
            }
        } message: {
            Text("Pair again to get it back.")
        }
    }

    /// The connection in the title's chip, coloured by how it is, with the last round trip
    /// while connected: "Connected · 38 ms".
    static func chip(_ machine: Machine) -> StatusChip {
        let urgency: Urgency = switch machine.connection {
        case .connected: .done
        case .connecting: .running
        default: .failing
        }
        let rtt = machine.connection == .connected ? machine.quality.lastRttMs.map { "\($0) ms" } : nil
        return StatusChip(text: [machine.connection.label, rtt].compactMap(\.self).joined(separator: " · "),
                          urgency: urgency)
    }

    /// "owner · 2 running": the user's role and what runs there.
    private func status(_ machine: Machine) -> String {
        let running = fleet.lists.machines.first { $0.hostId == hostId }?.running ?? 0
        let role = machine.role.map { $0 == .owner ? "owner" : "member" }
        return [role, running > 0 ? "\(running) running" : nil].compactMap(\.self).joined(separator: " · ")
    }

    // MARK: Needs attention

    /// The accounts signed out here, to sign in again, and the CLIs to update; for the owner,
    /// as only they may.
    @ViewBuilder private func attention(_ machine: Machine) -> some View {
        let attention = MachineAttention(machine: machine, versions: ProviderVersions(machines: fleet.machines))
        let canFix = machine.role == .owner && machine.connection == .connected
        if !attention.isEmpty {
            NeedsAttention {
                ForEach(Array(attention.signedOut.enumerated()), id: \.element.id) { index, signedOut in
                    if index > 0 { RowDivider() }
                    let account = signedOut.account
                    AttentionRow(title: "\(ModelCatalog.providerName(account.provider)) · \(account.label) is signed out",
                                 detail: MachineAttention.detail(signedOut, now: .now)) {
                        PillButton(title: "Sign in again", prominent: true) {
                            fleet.relogin(account.accountId, on: hostId)
                        }
                        .disabled(!canFix || fleet.activity.phase(JobKey(hostId: hostId, kind: .relogin(account.accountId))) != .ended)
                    }
                }
                if !attention.updates.isEmpty {
                    if !attention.signedOut.isEmpty { RowDivider() }
                    AttentionRow(title: attention.updatesTitle, detail: attention.updatesDetail) {
                        PillButton(title: attention.updateTitle) {
                            for cell in attention.updates { fleet.install(cell.provider, on: hostId) }
                        }
                        .disabled(attention.updates.allSatisfy {
                            fleet.activity.phase(JobKey(hostId: hostId, kind: .install($0.provider))) != .ended
                        })
                    }
                }
            }
        }
    }

    // MARK: Accounts

    /// The machine's accounts, each with its tightest limit and opening its page; adding one;
    /// and the logins its other machines have that it lacks, to set up here.
    private func accounts(_ machine: Machine) -> some View {
        let owner = machine.role == .owner
        let connected = machine.connection == .connected
        let logins = Dictionary(ProviderAccounts(machines: fleet.machines).groups.flatMap(\.logins).map { ($0.id, $0) },
                                uniquingKeysWith: { first, _ in first })
        let missing = SetUpGaps(ProviderAccounts(machines: fleet.machines)).gaps.compactMap { gap in
            gap.machines.first { $0.hostId == hostId }.map { (login: gap.login, place: $0) }
        }
        let running = fleet.activity.pendingIds(on: hostId).count
        return VStack(alignment: .leading, spacing: 8) {
            SettingsList(title: "Accounts") {
                if machine.accounts.isEmpty {
                    Text("No accounts yet").font(.subheadline).foregroundStyle(Theme.tertiary)
                        .padding(.horizontal, 14).frame(minHeight: 44)
                }
                ForEach(Array(machine.accounts.enumerated()), id: \.element.accountId) { index, account in
                    if index > 0 { RowDivider() }
                    accountRow(account, login: logins[ProviderAccounts.key(account)])
                }
                ForEach(missing, id: \.login.id) { gap in
                    RowDivider()
                    HStack(spacing: 10) {
                        ProviderMark(provider: gap.login.provider, size: 16).frame(width: 18).opacity(0.6)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("\(ModelCatalog.providerName(gap.login.provider)) · \(gap.login.email ?? gap.login.account.label)")
                                .font(.subheadline).foregroundStyle(Theme.secondary).lineLimit(1)
                            Text("Missing here · on \(gap.login.on.map(\.name).joined(separator: ", "))")
                                .font(.caption).foregroundStyle(Theme.tertiary).lineLimit(1)
                        }
                        Spacer(minLength: 8)
                        if owner {
                            PillButton(title: gap.place.needsInstall ? "Install & Set Up" : "Set Up") {
                                let taken = AccountDraft.taken(on: machine, pending: fleet.activity.pendingIds(on: hostId))
                                if gap.place.needsInstall { fleet.install(gap.login.provider, on: hostId) }
                                setup = ProvidersView.Setup(hostId: hostId, draft: gap.login.draft(taken: taken))
                            }
                            .disabled(!connected)
                        }
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
                    .frame(minHeight: 48)
                }
                RowDivider()
                HStack(spacing: 10) {
                    if owner {
                        AddRow(title: "Add account…") { addingAccount = true }
                            .disabled(!connected)
                            .accessibilityLabel("Add Account")
                    } else {
                        Text("Only the machine’s owner can add accounts.").font(.footnote).foregroundStyle(Theme.tertiary)
                            .padding(.leading, Theme.cellPadding)
                            .frame(maxWidth: .infinity, minHeight: Theme.rowHeight, alignment: .leading)
                    }
                    if owner && running > 0 {
                        Text(running == 1 ? "1 login running" : "\(running) logins running")
                            .font(.caption).foregroundStyle(Theme.secondary).fixedSize()
                    }
                    if machine.failover.pin {
                        Label("Failover pinned", systemImage: "pin").font(.caption).foregroundStyle(Theme.tertiary)
                            .help("Sessions stay on their account when it hits a limit.")
                            .fixedSize()
                    }
                }
                .padding(.trailing, Theme.cellPadding)
            }
            Text("A session at its limit rotates to another of its provider’s accounts here.")
                .font(.footnote).foregroundStyle(Theme.tertiary)
        }
    }

    /// An account: who it is, then signed out or what is left of its tightest limit; it opens
    /// the account's page.
    private func accountRow(_ account: Account, login: ProviderAccounts.Login?) -> some View {
        Button {
            pages.open(.account(loginId: ProviderAccounts.key(account)))
        } label: {
            HStack(spacing: 10) {
                ProviderMark(provider: account.provider, size: 16).frame(width: 18)
                VStack(alignment: .leading, spacing: 2) {
                    Text("\(ModelCatalog.providerName(account.provider)) · \(account.label)")
                        .font(.subheadline).foregroundStyle(Theme.text).lineLimit(1)
                    Text(account.email.map { "\($0) · \(account.accountId)" } ?? account.accountId)
                        .font(Theme.monoSmall).foregroundStyle(Theme.tertiary).lineLimit(1)
                }
                if account.fallback { Badge(text: "Fallback") }
                Spacer(minLength: 8)
                if account.needsLogin != nil {
                    Text("Signed out").font(.caption.weight(.semibold)).foregroundStyle(Theme.accent)
                } else if let tightest = login?.tightest {
                    HeadroomMeter(window: tightest.window).frame(width: 72)
                    Text("\(Int(tightest.window.percentLeft.rounded()))% \(tightest.label.lowercased()) left")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(tightest.window.headroom == .plenty ? Theme.secondary : tightest.window.headroom.color)
                        .lineLimit(1)
                }
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            .frame(minHeight: 48)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("machine-account-\(account.accountId)")
    }

    // MARK: Provider CLIs

    /// The provider CLIs here: each one's version against the newest your machines have, and
    /// its install or update.
    @ViewBuilder private func clis(_ machine: Machine) -> some View {
        let rows = ProviderVersions(machines: fleet.machines).rows
        if !rows.isEmpty {
            SettingsList(title: "Provider CLIs") {
                ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                    if index > 0 { RowDivider() }
                    if let cell = row.cells.first(where: { $0.hostId == hostId }) {
                        HStack(spacing: 10) {
                            ProviderMark(provider: row.provider, size: 16).frame(width: 18)
                            Text(ModelCatalog.providerName(row.provider)).font(.subheadline).foregroundStyle(Theme.text)
                            Spacer(minLength: 8)
                            if cell.state == .outdated, let available = cell.available {
                                Text("Update available → \(available)").font(.caption).foregroundStyle(Theme.secondary)
                            }
                            VersionCell(cell: cell, showsAvailable: false) { cell in
                                fleet.install(cell.provider, on: cell.hostId)
                            }
                        }
                        .padding(.horizontal, 14)
                        .frame(minHeight: 44)
                    }
                }
            }
            .detailSection("Provider CLIs")
        }
    }

    // MARK: herder

    /// The herder the machine runs, and its Update.
    private func herder(_ machine: Machine) -> some View {
        SettingsList(title: "herder") {
            SettingRow(label: "Version", hint: machine.herderVersion ?? "Too old to say") {
                HerderUpdateButton(fleet: fleet, hostId: hostId)
            }
        }
    }

    // MARK: Capacity

    private func capacity(_ machine: Machine, _ resources: HostResources) -> some View {
        SettingsList(title: "Capacity") {
            TurnLimitRow(fleet: fleet, hostId: hostId, resources: resources,
                         canChange: machine.role == .owner && machine.connection == .connected)
        }
    }

    // MARK: Connection

    /// The machine's name here and its addresses, which unfold to edit them, with the link's
    /// health since the app opened.
    private func connection(_ machine: Machine) -> some View {
        SettingsList(title: "Connection") {
            SettingRow(label: "Name on this device", hint: "Only this device shows it", saved: saves["Name"]) {
                TextField(machine.name, text: $name)
                    .textFieldStyle(.plain)
                    .foregroundStyle(Theme.text)
                    .multilineTextAlignment(.trailing)
                    .autocorrectionDisabled()
                    .focused($editingName)
                    .onSubmit { rename() }
                    .padding(.horizontal, 10)
                    .frame(maxWidth: 240, minHeight: 32)
                    .background(Theme.surface, in: .rect(cornerRadius: 7))
                    .accessibilityIdentifier("machine-name")
            }
            RowDivider()
            Button { withAnimation(.easeInOut(duration: 0.15)) { showingAddresses.toggle() } } label: {
                SettingRow(label: "Addresses", hint: machine.address.map { "Connected through \($0)" } ?? "") {
                    Text(machine.addresses.count == 1 ? "1 address" : "\(machine.addresses.count) addresses")
                        .font(.subheadline).foregroundStyle(Theme.secondary)
                    Image(systemName: "chevron.right")
                        .font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                        .rotationEffect(.degrees(showingAddresses ? 90 : 0))
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("machine-addresses")
            if showingAddresses {
                VStack(alignment: .leading, spacing: 14) {
                    Text("Tried first to last: each gets a head start over the ones below it, so the first that answers wins. Connect moves an address to the top and reconnects through it now. The port defaults to 7447.")
                        .font(.caption).foregroundStyle(Theme.tertiary)
                        .fixedSize(horizontal: false, vertical: true)
                    addresses(machine)
                    link(machine)
                }
                .padding([.horizontal, .bottom], 14)
            }
        }
    }

    /// Renames the machine on this device once the name is left, unless it is blank or the same.
    private func rename() {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty, trimmed != fleet.machines.first(where: { $0.hostId == hostId })?.name else { return }
        Task { await saves.save("Name") { try fleet.rename(hostId, to: trimmed) } }
    }

    /// The link's health since the app opened: uptime, reconnects, round trips and each change.
    private func link(_ machine: Machine) -> some View {
        let log = fleet.connectionLog[hostId] ?? []
        let health = ConnectionHealth(log: log, now: .now)
        return VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 18) {
                stat("Up for", health.currentUp.map(Self.duration) ?? "—")
                stat("Reconnects", "\(health.reconnects)")
                stat("Disconnected", Self.duration(health.down))
            }
            .padding(.bottom, 4)
            LinkQualityView(quality: machine.quality, roundTrips: fleet.roundTrips[hostId] ?? [])
                .padding(.bottom, 4)
            ForEach(Array(ConnectionHealth.runs(log, now: .now).reversed().enumerated()), id: \.offset) { _, run in
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    ConnectionMark(state: run.state)
                    Text(run.at.formatted(date: .omitted, time: .standard))
                        .monospacedDigit().foregroundStyle(Theme.tertiary)
                    Text(run.state.label).foregroundStyle(Theme.text).lineLimit(2)
                    if run.times > 1 {
                        Text("×\(run.times)").font(.caption.weight(.semibold)).foregroundStyle(Theme.secondary)
                    }
                    Spacer()
                    Text(Self.duration(run.lasted)).monospacedDigit().foregroundStyle(Theme.tertiary)
                }
                .font(.footnote)
            }
        }
        .padding(12)
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
    }

    /// The machine's addresses in order, each movable, removable and one to connect through now, and
    /// a field to add one.
    private func addresses(_ machine: Machine) -> some View {
        let addresses = machine.addresses
        return VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(addresses.enumerated()), id: \.element) { index, address in
                AddressRow(index: index, address: address, count: addresses.count,
                           status: AddressStatus(address: address, inUse: machine.address,
                                                 connecting: connecting, failure: connectFailure),
                           busy: connecting != nil,
                           connect: { connect(through: address) },
                           move: { move(addresses, from: index, to: $0) },
                           remove: { perform { try fleet.setAddresses(hostId, to: addresses.filter { $0 != address }) } })
            }
            HStack(spacing: 10) {
                InputBox(placeholder: "Host or IP, e.g. box.tailnet.ts.net", text: $newAddress, mono: true)
                ActionButton(title: "Add", style: .secondary) {
                    perform {
                        try fleet.setAddresses(hostId, to: addresses + [newAddress.trimmingCharacters(in: .whitespaces)])
                        newAddress = ""
                    }
                }
                .frame(width: 120)
                .disabled(newAddress.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
    }

    private func connect(through address: String) {
        connecting = address
        connectFailure = nil
        Task {
            do {
                try await fleet.connect(hostId, through: address)
            } catch {
                connectFailure = .init(address: address, message: describe(error))
            }
            connecting = nil
        }
    }

    private func move(_ addresses: [String], from: Int, to: Int) {
        var addresses = addresses
        addresses.swapAt(from, to)
        perform { try fleet.setAddresses(hostId, to: addresses) }
    }

    private func perform(_ action: () throws -> Void) {
        do {
            try action()
            error = nil
        } catch {
            self.error = describe(error)
        }
    }

    private func stat(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(value).font(.headline.monospacedDigit()).foregroundStyle(Theme.text)
            Text(label).font(.caption).foregroundStyle(Theme.tertiary)
        }
    }

    static func duration(_ seconds: TimeInterval) -> String {
        Duration.seconds(max(0, seconds)).formatted(.units(allowed: [.hours, .minutes, .seconds], width: .abbreviated, maximumUnitCount: 2))
    }
}
