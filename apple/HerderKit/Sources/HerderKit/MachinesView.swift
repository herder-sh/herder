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

/// The machines as a table, each row opening its page, then the vaults they back up to; on
/// compact width also the way to Skills and Providers.
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
                    }
                }
                SortableTable("machines", rows: MachineRow.rows(fleet.machines, summaries: fleet.lists.machines),
                              sortedBy: TableSort(column: "Machine"), columns: columns) { pages.open(.machine($0.hostId)) }
                let vaults = fleet.vaults
                if !vaults.isEmpty || !unpaired.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        SectionHeading(title: "Vault")
                        VStack(spacing: 0) {
                            ForEach(vaults) { vault in
                                VaultRow(vault: vault) { pages.open(.vault(vault.hostId)) }
                            }
                            ForEach(unpaired) { vault in
                                UnpairedVaultRow(vault: vault) { sheet = .pair }
                            }
                        }
                        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
                        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
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

    private var columns: [SortableColumn<MachineRow>] {
        var columns: [SortableColumn<MachineRow>] = [
            SortableColumn("Machine", value: \.name) { row in
                VStack(alignment: .leading, spacing: 1) {
                    Text(row.name).fontWeight(.semibold).lineLimit(1)
                    let detail = [row.local ? "this Mac" : nil, row.role].compactMap(\.self).joined(separator: " · ")
                    if !detail.isEmpty {
                        Text(detail).font(Theme.meta).foregroundStyle(Theme.tertiary).lineLimit(1)
                    }
                }
            },
            SortableColumn("Status", width: 110, value: \.statusRank) { row in
                HStack(spacing: 6) {
                    ConnectionMark(state: row.connection)
                    Text(Self.status(row)).foregroundStyle(Theme.secondary).monospacedDigit().lineLimit(1)
                }
                .help(row.connection.label)
            },
            SortableColumn("Running", width: 64, alignment: .trailing, value: \.running) { row in
                Text("\(row.running)").monospacedDigit().foregroundStyle(row.running > 0 ? Theme.running : Theme.tertiary)
            },
        ]
        guard !compact else { return columns }
        columns += [
            SortableColumn("Load", width: 90, value: { $0.cpu ?? -1 }) { row in
                if let cpu = row.cpu {
                    UsageBar(percent: cpu, height: 5)
                        .help("CPU \(Int(cpu.rounded()))%" + (row.memory.map { " · memory \(Int($0.rounded()))%" } ?? ""))
                } else {
                    Text("–").foregroundStyle(Theme.tertiary)
                }
            },
            SortableColumn("CLIs", width: 100, value: { $0.updates ?? -1 }) { row in
                switch row.updates {
                case nil: Text("–").foregroundStyle(Theme.tertiary)
                case 0?: Chip(text: "current", tint: Theme.success)
                case let updates?: Chip(text: updates == 1 ? "1 update" : "\(updates) updates", tint: Theme.accent)
                }
            },
            SortableColumn("Accounts", width: 150, value: \.accounts) { row in
                HStack(spacing: 6) {
                    Text("\(row.accounts)").monospacedDigit()
                    if row.signedOut > 0 { Chip(text: "\(row.signedOut) signed out", tint: Theme.accent) }
                }
            },
            SortableColumn("Backs up to", width: 140, value: { $0.backsUpTo ?? "" }) { row in
                Text(row.backsUpTo ?? "–").foregroundStyle(row.backsUpTo == nil ? Theme.tertiary : Theme.secondary)
                    .lineLimit(1)
            },
        ]
        return columns
    }

    /// "12 ms" while connected, else the connection's state in a word or two.
    static func status(_ row: MachineRow) -> String {
        switch row.connection {
        case .connected: row.latencyMs.map { "\($0) ms" } ?? "Connected"
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

/// A paired vault in Machines: its hosts, sessions and size, and the host furthest behind;
/// it opens the vault's page.
struct VaultRow: View {
    let vault: VaultStats
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 12) {
                Image(systemName: "archivebox").font(.title3)
                    .foregroundStyle(vault.connection == .connected ? Theme.text : Theme.tertiary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(vault.name).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    Text(Self.summary(vault)).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                }
                Spacer(minLength: 12)
                if let host = vault.furthestBehind {
                    Chip(text: "\(host.name) \(host.freshness.label(now: vault.now))", tint: Theme.accent)
                } else if !vault.hosts.isEmpty {
                    Chip(text: "✓ caught up", tint: Theme.success)
                }
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .frame(minHeight: 52)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("vault-row")
    }

    /// "2 hosts · 522 sessions · 245 MB".
    static func summary(_ vault: VaultStats) -> String {
        [vault.hosts.count == 1 ? "1 host" : "\(vault.hosts.count) hosts",
         vault.sessions == 1 ? "1 session" : "\(vault.sessions) sessions",
         vault.size].compactMap(\.self).joined(separator: " · ")
    }
}

/// A vault this device is not paired with: who backs up to it, and how to pair with it.
private struct UnpairedVaultRow: View {
    let vault: UnpairedVault
    let pair: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "archivebox").font(.title3).foregroundStyle(Theme.tertiary)
            VStack(alignment: .leading, spacing: 2) {
                Text(vault.address).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                Text("\(vault.backsUp) · not paired with this device").font(.caption).foregroundStyle(Theme.secondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 12)
            InfoButton(text: "Its statistics show once this device is paired with it. On the vault, run `herder pair`, then add the link it prints as a machine.")
            PillButton(title: "Add Machine", symbol: "plus", action: pair)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: 52)
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
