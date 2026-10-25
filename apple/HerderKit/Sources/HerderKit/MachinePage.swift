import Herder
import SwiftUI

/// A machine's page: its accounts, provider CLIs and MCP servers, its turn limit, backup and
/// connection, then its identifiers and forgetting it.
struct MachinePage: View {
    let fleet: Fleet
    let hostId: HostId
    @Environment(\.pages) private var pages
    @State private var name = ""
    @State private var newAddress = ""
    @State private var editingAccount: Account?
    @State private var showingAccountSettings = false
    @State private var addingAccount = false
    @State private var setup: ProvidersView.Setup?
    @State private var confirmingForget = false
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
                if let resources = machine.resources, machine.hosts.isEmpty {
                    TurnLimitField(fleet: fleet, hostId: hostId, load: TurnLoad(resources),
                                   canChange: machine.role == .owner && machine.connection == .connected)
                        .detailSection("Capacity")
                }
                if machine.hosts.isEmpty {
                    accounts(machine).detailSection("Accounts")
                    clis(machine)
                    McpMachineSettings(fleet: fleet, machine: machine).detailSection("MCP servers")
                }
                if machine.role == .owner && machine.connection == .connected && machine.hosts.isEmpty {
                    BackupField(fleet: fleet, machine: machine).detailSection("Backup")
                }
                connection(machine).detailSection("Connection")
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Theme.failure)
                }
                AboutSection {
                    DetailRow(label: "Your role", value: machine.role.map { $0 == .owner ? "Owner" : "Member" }
                              ?? "Known once connected")
                    DetailRow(label: "Fingerprint", value: grouped(machine.fingerprint), mono: true)
                    DetailRow(label: "Host id", value: machine.hostId, mono: true)
                    if !machine.hosts.isEmpty {
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
        .sheet(isPresented: $showingAccountSettings) {
            if let editingAccount { EditAccountSheet(fleet: fleet, hostId: hostId, account: editingAccount) }
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

    /// The connection in the title's chip, coloured by how it is.
    static func chip(_ machine: Machine) -> StatusChip {
        let urgency: Urgency = switch machine.connection {
        case .connected: .done
        case .connecting: .running
        default: .failing
        }
        return StatusChip(text: machine.connection.label, urgency: urgency)
    }

    /// "owner · 2 running": the user's role and what runs there.
    private func status(_ machine: Machine) -> String {
        let running = fleet.lists.machines.first { $0.hostId == hostId }?.running ?? 0
        let role = machine.role.map { $0 == .owner ? "owner" : "member" }
        return [role, running > 0 ? "\(running) running" : nil].compactMap(\.self).joined(separator: " · ")
    }

    /// The machine's name here, its link's health and its addresses.
    private func connection(_ machine: Machine) -> some View {
        VStack(alignment: .leading, spacing: 18) {
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
            }
            Field(label: "Link", hint: "Since the app opened. Round trips are pings every 15 seconds.") {
                let log = fleet.connectionLog[hostId] ?? []
                let health = ConnectionHealth(log: log, now: .now)
                VStack(alignment: .leading, spacing: 8) {
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
                .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
            }
            Field(label: "Addresses",
                  hint: "Tried first to last: each gets a head start over the ones below it, so the first that answers wins. Connect moves an address to the top and reconnects through it now. The port defaults to 7447.") {
                addresses(machine)
            }
        }
    }

    /// Renames the machine on this device once the name is left, unless it is blank or the same.
    private func rename() {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty, trimmed != fleet.machines.first(where: { $0.hostId == hostId })?.name else { return }
        Task { await saves.save("Name") { try fleet.rename(hostId, to: trimmed) } }
    }

    /// The machine's accounts, each to edit; adding one; and the logins its other machines have
    /// that it lacks, to set up here.
    private func accounts(_ machine: Machine) -> some View {
        let owner = machine.role == .owner
        let connected = machine.connection == .connected
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
                    HStack(spacing: 10) {
                        ProviderMark(provider: account.provider, size: 16).frame(width: 18)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(account.email ?? account.label).font(.subheadline).foregroundStyle(Theme.text).lineLimit(1)
                            Text(account.email == nil ? account.accountId : "\(account.label) · \(account.accountId)")
                                .font(Theme.monoSmall).foregroundStyle(Theme.tertiary).lineLimit(1)
                        }
                        if account.fallback { Badge(text: "Fallback") }
                        Spacer(minLength: 8)
                        if owner {
                            PillButton(title: "Edit…") {
                                editingAccount = account
                                showingAccountSettings = true
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
                        PillButton(title: "Add Account", symbol: "plus") { addingAccount = true }.disabled(!connected)
                        if running > 0 {
                            Text(running == 1 ? "1 login running" : "\(running) logins running")
                                .font(.caption).foregroundStyle(Theme.accent)
                        }
                    } else {
                        Text("Only the machine’s owner can add accounts.").font(.footnote).foregroundStyle(Theme.tertiary)
                    }
                    Spacer()
                    if machine.failover.pin {
                        Label("Failover pinned", systemImage: "pin").font(.caption).foregroundStyle(Theme.tertiary)
                            .help("Sessions stay on their account when it hits a limit.")
                    }
                }
                .padding(.horizontal, 14)
                .frame(minHeight: 44)
            }
            if !missing.isEmpty {
                SettingsList(title: "On your other machines") {
                    ForEach(Array(missing.enumerated()), id: \.element.login.id) { index, gap in
                        if index > 0 { RowDivider() }
                        HStack(spacing: 10) {
                            ProviderMark(provider: gap.login.provider, size: 16).frame(width: 18)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(gap.login.email ?? gap.login.account.label).font(.subheadline)
                                    .foregroundStyle(Theme.text).lineLimit(1)
                                Text("On \(gap.login.on.map(\.name).joined(separator: ", "))").font(.caption)
                                    .foregroundStyle(Theme.tertiary).lineLimit(1)
                            }
                            Spacer(minLength: 8)
                            PillButton(title: gap.place.needsInstall ? "Install & Set Up" : "Set Up") {
                                let taken = AccountDraft.taken(on: machine, pending: fleet.activity.pendingIds(on: hostId))
                                if gap.place.needsInstall { fleet.install(gap.login.provider, on: hostId) }
                                setup = ProvidersView.Setup(hostId: hostId, draft: gap.login.draft(taken: taken))
                            }
                            .disabled(!connected)
                        }
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .frame(minHeight: 48)
                    }
                }
            }
            Text("A session at its limit rotates to another of its provider’s accounts here.")
                .font(.footnote).foregroundStyle(Theme.tertiary)
        }
    }

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
                                Text("Update available → \(available)").font(.caption).foregroundStyle(Theme.accent)
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
        .padding(12)
        .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
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
