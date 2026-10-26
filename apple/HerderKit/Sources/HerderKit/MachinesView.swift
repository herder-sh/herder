import Foundation
import Herder
import SwiftUI

/// A machine as a row of the Machines table: what to act on, at a glance.
struct MachineRow: Hashable, Identifiable {
    var id: HostId { hostId }
    let hostId: HostId
    let name: String
    /// "owner", "member".
    let role: String?
    /// Whether the daemon runs on this Mac.
    let local: Bool
    let connection: ConnectionState
    /// The latest ping's round trip.
    let latencyMs: UInt32?
    let running: Int
    /// Percentages, once the daemon has reported its resources.
    let cpu: Double?
    let memory: Double?
    /// Its provider CLIs that are behind; nil when it reports none.
    let updates: Int?
    let accounts: Int
    let signedOut: Int
    /// The paired vault it backs up to, by name.
    let backsUpTo: String?

    /// A row per machine that runs sessions; vaults show in their own section.
    static func rows(_ machines: [Machine], summaries: [MachineSummary], localNames: Set<String> = localNames) -> [MachineRow] {
        let versions = ProviderVersions(machines: machines)
        let vaults = machines.filter { !$0.hosts.isEmpty || $0.vault != nil }
        return machines.filter { $0.hosts.isEmpty && $0.vault == nil }.map { machine in
            let summary = summaries.first { $0.hostId == machine.hostId }
            let clis = versions.rows.flatMap(\.cells).filter { $0.hostId == machine.hostId && $0.state != .missing }
            return MachineRow(
                hostId: machine.hostId, name: machine.name,
                role: machine.role.map { $0 == .owner ? "owner" : "member" },
                local: isLocal(machine.address, names: localNames),
                connection: machine.connection,
                latencyMs: machine.connection == .connected ? machine.quality.lastRttMs : nil,
                running: summary?.running ?? 0, cpu: summary?.cpu, memory: summary?.memory,
                updates: clis.isEmpty ? nil : clis.filter { $0.state == .outdated }.count,
                accounts: machine.accounts.count,
                signedOut: machine.accounts.filter { $0.needsLogin != nil }.count,
                backsUpTo: vaults.first { $0.hosts.contains { $0.hostId == machine.hostId } }?.name)
        }
    }

    /// The names this Mac answers to, lowercased: loopback and its host name.
    static var localNames: Set<String> {
        #if os(macOS)
        let host = ProcessInfo.processInfo.hostName.lowercased()
        return ["localhost", "127.0.0.1", "::1", host, host.replacingOccurrences(of: ".local", with: "")]
        #else
        return []
        #endif
    }

    /// Whether `address`, "host:port" or "[v6]:port", names one of `names`.
    static func isLocal(_ address: String?, names: Set<String>) -> Bool {
        guard let address else { return false }
        let host = if address.hasPrefix("["), let close = address.firstIndex(of: "]") {
            String(address[address.index(after: address.startIndex)..<close])
        } else {
            String(address.split(separator: ":").first ?? "")
        }
        return names.contains(host.lowercased())
    }

    /// Connected first, then by round trip.
    var statusRank: Double {
        switch connection {
        case .connected: Double(latencyMs ?? 0)
        case .connecting: 1_000_000
        case .disconnected: 2_000_000
        }
    }
}

/// The machines as a table, each row opening its page, then the vaults they back up to in a
/// table of the same style; on compact width also the way to Skills and Providers.
struct MachinesView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    /// Leads to Skills and Providers, where they have no section of their own.
    var showsSections = false
    @State private var unpaired: [UnpairedVault] = []
    @Environment(\.pages) private var pages
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                if showsSections {
                    VStack(spacing: 10) {
                        skillsLink
                        providersLink
                        integrationsLink
                    }
                }
                let rows = MachineRow.rows(fleet.machines, summaries: fleet.lists.machines)
                let vaults = fleet.vaults.map(VaultLine.paired) + unpaired.map(VaultLine.unpaired)
                if rows.isEmpty && vaults.isEmpty {
                    ContentUnavailableView {
                        Label("No machines", systemImage: "server.rack")
                    } description: {
                        Text("Run herder on a machine, then pair this device with it.")
                    } actions: {
                        PillButton(title: "Add Machine", symbol: "plus", prominent: true) { sheet = .pair }
                    }
                    .padding(.top, 60)
                } else {
                    SortableTable("machines", rows: rows, sortedBy: TableSort(column: "Machine"),
                                  columns: columns) { pages.open(.machine($0.hostId)) }
                }
                if !vaults.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        SectionHeading(title: "Vault")
                        SortableTable("vaults", rows: vaults, sortedBy: TableSort(column: "Vault"),
                                      columns: vaultColumns) { line in
                            switch line {
                            case .paired(let vault): pages.open(.vault(vault.hostId))
                            case .unpaired: sheet = .pair
                            }
                        }
                    }
                }
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 24)
        }
        .background(Theme.background)
        .refreshable { fleet.wake() }
        // Asked again as machines connect, and as a vault gets paired.
        .task(id: fleet.machines.filter { $0.connection == .connected }.map(\.hostId)) {
            unpaired = await fleet.unpairedVaults()
        }
        .navigationTitle("Machines")
        .toolbar {
            #if os(iOS)
            ActivityToolbarButton(fleet: fleet) { sheet = .activity }
            Button("Reconnect", systemImage: "arrow.clockwise") { fleet.wake() }
            if !AwsSignIn.machines(fleet.machines).isEmpty {
                Button("AWS Sign-in", systemImage: "key") { sheet = .awsSignIn }
            }
            Button("Add Machine", systemImage: "plus") { sheet = .pair }
            #endif
        }
    }

    private var compact: Bool {
        #if os(iOS)
        sizeClass == .compact
        #else
        false
        #endif
    }

    /// The widths both tables share, so their first columns line up.
    private enum Width {
        static let name: CGFloat = 200
        static let status: CGFloat = 100
        static let count: CGFloat = 70
    }

    private var columns: [SortableColumn<MachineRow>] {
        var columns: [SortableColumn<MachineRow>] = [
            SortableColumn("Machine", width: compact ? nil : Width.name, value: \.name) { row in
                VStack(alignment: .leading, spacing: 1) {
                    Text(row.name).fontWeight(.semibold).lineLimit(1)
                    let detail = [row.local ? "this Mac" : nil, row.role].compactMap(\.self).joined(separator: " · ")
                    if !detail.isEmpty {
                        Text(detail).font(Theme.meta).foregroundStyle(Theme.tertiary).lineLimit(1)
                    }
                }
            },
            SortableColumn("Status", width: Width.status, value: \.statusRank) { row in
                StatusCell(connection: row.connection, text: Self.status(row))
            },
            SortableColumn("Running", width: Width.count, alignment: .trailing, value: \.running) { row in
                HStack(spacing: 5) {
                    if row.running > 0 { Circle().fill(Theme.running).frame(width: 6, height: 6) }
                    Text("\(row.running)").monospacedDigit()
                        .foregroundStyle(row.running > 0 ? Theme.text : Theme.tertiary)
                }
            },
        ]
        guard !compact else { return columns }
        columns += [
            SortableColumn("Load", width: 120, alignment: .trailing, value: { $0.cpu ?? -1 }) { row in
                if let cpu = row.cpu {
                    HStack(spacing: 8) {
                        UsageBar(percent: cpu, height: 5).frame(width: 60)
                        Text("\(Int(cpu.rounded()))%").monospacedDigit().foregroundStyle(Theme.secondary)
                            .frame(width: 38, alignment: .trailing)
                    }
                    .help("CPU \(Int(cpu.rounded()))%" + (row.memory.map { " · memory \(Int($0.rounded()))%" } ?? ""))
                } else {
                    Text("–").foregroundStyle(Theme.tertiary)
                        .help(row.connection == .connected ? "This machine does not report its load." : "Offline")
                }
            },
            SortableColumn("CLIs", width: 110, value: { $0.updates ?? -1 }) { row in
                switch row.updates {
                case nil: Text("–").foregroundStyle(Theme.tertiary)
                case 0?: Text("current").foregroundStyle(Theme.tertiary)
                case let updates?:
                    Button { pages.open(.machine(row.hostId)) } label: {
                        Chip(text: updates == 1 ? "1 update" : "\(updates) updates")
                    }
                    .buttonStyle(.plain)
                    .help("Update on its page")
                }
            },
            SortableColumn("Accounts", width: 150, alignment: .trailing, value: \.accounts) { row in
                HStack(spacing: 8) {
                    if row.signedOut > 0 { Chip(text: "\(row.signedOut) signed out", tint: Theme.accent) }
                    Text("\(row.accounts)").monospacedDigit()
                }
            },
            SortableColumn("Backs up to", value: { $0.backsUpTo ?? "" }) { row in
                Text(row.backsUpTo ?? "–").foregroundStyle(row.backsUpTo == nil ? Theme.tertiary : Theme.secondary)
                    .lineLimit(1)
            },
        ]
        return columns
    }

    private var vaultColumns: [SortableColumn<VaultLine>] {
        var columns: [SortableColumn<VaultLine>] = [
            SortableColumn("Vault", width: compact ? nil : Width.name, value: \.name) { line in
                VStack(alignment: .leading, spacing: 1) {
                    Text(line.name).fontWeight(.semibold).lineLimit(1)
                    if case .unpaired = line {
                        Text("not paired with this device").font(Theme.meta).foregroundStyle(Theme.tertiary).lineLimit(1)
                    }
                }
            },
        ]
        if !compact {
            columns += [
                SortableColumn("Status", width: Width.status) { line in
                    switch line {
                    case .paired(let vault):
                        StatusCell(connection: vault.connection, text: Self.status(vault.connection, latencyMs: nil))
                    case .unpaired:
                        Text("–").foregroundStyle(Theme.tertiary)
                    }
                },
                SortableColumn("Hosts", width: Width.count, alignment: .trailing, value: \.hosts) { line in
                    Text("\(line.hosts)").monospacedDigit()
                },
                SortableColumn("Sessions", width: 120, alignment: .trailing, value: { $0.sessions ?? -1 }) { line in
                    Text(line.sessions.map(String.init) ?? "–").monospacedDigit()
                        .foregroundStyle(line.sessions == nil ? Theme.tertiary : Theme.text)
                },
                SortableColumn("Size", width: 110, alignment: .trailing, value: { $0.bytes ?? 0 }) { line in
                    Text(line.size ?? "–").monospacedDigit().foregroundStyle(line.size == nil ? Theme.tertiary : Theme.text)
                },
            ]
        }
        columns.append(SortableColumn("Backup", value: { $0.behind ?? -1 }) { line in
            BackupCell(line: line) { sheet = .pair }
        })
        return columns
    }

    /// "12 ms" while connected, else the connection's state in a word or two.
    static func status(_ row: MachineRow) -> String {
        status(row.connection, latencyMs: row.latencyMs)
    }

    static func status(_ connection: ConnectionState, latencyMs: UInt32?) -> String {
        switch connection {
        case .connected: latencyMs.map { "\($0) ms" } ?? "Connected"
        case .connecting: "Connecting…"
        case .disconnected: "Offline"
        }
    }

    private var providersLink: some View {
        NavigationLink { ProvidersView(fleet: fleet, sheet: $sheet) } label: {
            Card(padding: 12) {
                HStack(spacing: 10) {
                    Image(systemName: "person.2").foregroundStyle(Theme.secondary)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Providers").font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                        let gaps = SetUpGaps(ProviderAccounts(machines: fleet.machines))
                        Text(gaps.isEmpty ? "Accounts, plan limits, usage and CLIs" : gaps.title)
                            .font(.caption).foregroundStyle(Theme.secondary)
                    }
                    Spacer()
                    Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                }
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("providers-link")
    }

    private var integrationsLink: some View {
        // A page on the stack, so the bot pages it opens push over it rather than replace it.
        Button { pages.open(.integrations) } label: {
            Card(padding: 12) {
                HStack(spacing: 10) {
                    Image(systemName: Switcher.integrationsSymbol).foregroundStyle(Theme.secondary)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Integrations").font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                        Text("Slack bots on your machines").font(.caption).foregroundStyle(Theme.secondary)
                    }
                    Spacer()
                    Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                }
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("integrations-link")
    }

    private var skillsLink: some View {
        NavigationLink { SkillsView(fleet: fleet, sheet: $sheet) } label: {
            Card(padding: 12) {
                HStack(spacing: 10) {
                    Image(systemName: "book.closed").foregroundStyle(Theme.secondary)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Skills").font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                        let count = SkillLibrary(fleet.machines).skills.count
                        Text(count == 1 ? "1 skill in the library" : "\(count) skills in the library")
                            .font(.caption).foregroundStyle(Theme.secondary)
                    }
                    Spacer()
                    Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                }
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("skills-link")
    }
}

/// A vault as a row of the Machines screen's vault table: one this device is paired with,
/// opening its page, or one its machines back up to that it is not, which it can pair with.
enum VaultLine: Hashable, Identifiable {
    case paired(VaultStats)
    case unpaired(UnpairedVault)

    /// A host further behind than this makes the backup need you.
    static let overdue: TimeInterval = 60 * 60

    var id: String {
        switch self {
        case .paired(let vault): "paired.\(vault.hostId)"
        case .unpaired(let vault): "unpaired.\(vault.fingerprint)"
        }
    }

    var name: String {
        switch self {
        case .paired(let vault): vault.name
        case .unpaired(let vault): vault.address
        }
    }

    var hosts: Int {
        switch self {
        case .paired(let vault): vault.hosts.count
        case .unpaired(let vault): vault.hosts.count
        }
    }

    /// Unknown until this device is paired with it.
    var sessions: Int? {
        if case .paired(let vault) = self { vault.sessions } else { nil }
    }

    var bytes: UInt64? {
        if case .paired(let vault) = self { vault.storageBytes } else { nil }
    }

    /// "245 MB".
    var size: String? {
        if case .paired(let vault) = self { vault.size } else { nil }
    }

    /// How far behind its furthest host is, in seconds; nil while unknown.
    var behind: TimeInterval? {
        guard case .paired(let vault) = self, !vault.hosts.isEmpty else { return nil }
        return vault.furthestBehind?.freshness.behind(now: vault.now) ?? 0
    }

    /// "✓ caught up", or "work-mbp 6 h behind" for the host furthest behind; nil while unknown.
    var freshness: String? {
        guard case .paired(let vault) = self, !vault.hosts.isEmpty else { return nil }
        guard let host = vault.furthestBehind else { return "✓ caught up" }
        return "\(host.name) \(host.freshness.label(now: vault.now))"
    }

    /// Why the host furthest behind is, for the freshness's hover.
    var why: String? {
        guard case .paired(let vault) = self, let host = vault.furthestBehind else { return nil }
        return host.freshness.why(host: host.name, now: vault.now)
    }

    /// Whether the backup is behind by more than `overdue`, which needs you.
    var isOverdue: Bool { (behind ?? 0) > Self.overdue }
}

/// A connection's mark and a word or two for it, as the Status column shows it.
private struct StatusCell: View {
    let connection: ConnectionState
    let text: String

    var body: some View {
        HStack(spacing: 6) {
            ConnectionMark(state: connection)
            Text(text).foregroundStyle(Theme.secondary).monospacedDigit().lineLimit(1)
        }
        .help(connection.label)
    }
}

/// How current a vault's backup is: neutral, amber only once overdue, and why on hover; for a
/// vault this device is not paired with, who backs up to it and the way to pair.
private struct BackupCell: View {
    let line: VaultLine
    let pair: () -> Void

    var body: some View {
        switch line {
        case .paired:
            if let freshness = line.freshness {
                Text(freshness)
                    .foregroundStyle(line.isOverdue ? Theme.accent : Theme.secondary)
                    .lineLimit(1)
                    .help(line.why ?? "Every host's sessions are backed up.")
            } else {
                Text("No host backs up here yet").foregroundStyle(Theme.tertiary).lineLimit(1)
            }
        case .unpaired(let vault):
            HStack(spacing: 6) {
                Text(vault.backsUp).foregroundStyle(Theme.tertiary).lineLimit(1)
                InfoButton(text: "Its statistics show once this device is paired with it. On the vault, run `herder pair`, then add the link it prints as a machine.")
                Spacer(minLength: 0)
                PillButton(title: "Add Machine", symbol: "plus", action: pair)
            }
        }
    }
}

/// A machine's connection state as a mark: filled when connected, a dashed ring while
/// connecting, a crossed circle when the connection failed.
struct ConnectionMark: View {
    let state: ConnectionState

    var body: some View {
        Group {
            switch state {
            case .connected:
                Circle().fill(Theme.success).frame(width: 7, height: 7)
            case .connecting:
                Circle().strokeBorder(Theme.waiting, style: StrokeStyle(lineWidth: 1.5, dash: [2, 2]))
                    .frame(width: 9, height: 9)
            case .disconnected:
                Circle().fill(Theme.failure).frame(width: 10, height: 10)
                    .overlay(Image(systemName: "xmark").font(.system(size: 5, weight: .heavy))
                        .foregroundStyle(Theme.background))
            }
        }
        .accessibilityLabel(state.label)
    }
}

extension ConnectionState {
    var label: String {
        switch self {
        case .connected: "Connected"
        case .connecting: "Connecting…"
        case .disconnected(let error): Self.explain(error)
        }
    }

    /// A disconnection as the user can act on it: a daemon on another protocol version needs
    /// updating, which the raw error buries among unreachable addresses.
    static func explain(_ error: String) -> String {
        guard let theirsAt = error.range(of: "this daemon speaks "),
              let oursAt = error.range(of: "protocol version ") else { return error }
        let theirs = error[theirsAt.upperBound...].prefix { $0.isNumber }
        let ours = error[oursAt.upperBound...].prefix { $0.isNumber }
        return "Runs a different herder (protocol \(theirs); this app speaks \(ours)). Update one of them to connect."
    }
}
