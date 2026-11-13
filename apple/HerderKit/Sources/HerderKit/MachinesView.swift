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
    /// Where this device reaches it, "host:port".
    var address: String?
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
        let machines = machines.filter { $0.hosts.isEmpty && $0.vault == nil }
        let local = thisMac(machines, names: localNames)
        return machines.map { machine in
            let summary = summaries.first { $0.hostId == machine.hostId }
            let clis = versions.rows.flatMap(\.cells).filter { $0.hostId == machine.hostId && $0.state != .missing }
            return MachineRow(
                hostId: machine.hostId, name: machine.name,
                role: machine.role.map { $0 == .owner ? "owner" : "member" },
                local: machine.hostId == local,
                address: machine.address,
                connection: machine.connection,
                latencyMs: machine.connection == .connected ? machine.quality.lastRttMs : nil,
                running: summary?.running ?? 0, cpu: summary?.cpu, memory: summary?.memory,
                updates: clis.isEmpty ? nil : clis.filter { $0.state == .outdated }.count,
                accounts: machine.accounts.count,
                signedOut: machine.accounts.filter { $0.needsLogin != nil }.count,
                backsUpTo: vaults.first { $0.hosts.contains { $0.hostId == machine.hostId } }?.name)
        }
    }

    /// The names this Mac answers to, lowercased: loopback and its host name, as the kernel has
    /// it, which is what a daemon here names its machine. Read once, and with no lookup:
    /// `ProcessInfo.hostName` resolves the name over DNS and mDNS on the calling thread, and the
    /// sidebar reads this on every draw. Where no resolver answers, as on CI's Macs, each read
    /// held the main thread for seconds, and the mDNS query asked for Local Network access.
    static let localNames: Set<String> = {
        #if os(macOS)
        var buffer = [CChar](repeating: 0, count: Int(MAXHOSTNAMELEN) + 1)
        guard gethostname(&buffer, buffer.count - 1) == 0 else { return loopback }
        return names(host: String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self))
        #else
        return []
        #endif
    }()

    /// Loopback and `host`, lowercased, with and without ".local".
    static func names(host: String) -> Set<String> {
        let host = host.lowercased()
        let bare = host.hasSuffix(".local") ? String(host.dropLast(".local".count)) : host
        return loopback.union([host, bare, bare + ".local"])
    }

    /// The machine whose daemon runs on this Mac: of those reached at an address that names it,
    /// the one named after it, else the first paired. Several daemons can listen on loopback (a
    /// demo fleet, a tunnel), and only one of them is this Mac's.
    static func thisMac(_ machines: [Machine], names: Set<String>) -> HostId? {
        let local = machines.filter { isLocal($0.address, names: names) }
        return (local.first { names.contains($0.name.lowercased()) } ?? local.first)?.hostId
    }

    /// Whether `address`, "host:port" or "[v6]:port", names one of `names`.
    static func isLocal(_ address: String?, names: Set<String>) -> Bool {
        guard let address else { return false }
        return names.contains(host(of: address).lowercased())
    }

    /// The host of `address`, "host:port" or "[v6]:port", without its port.
    static func host(of address: String) -> String {
        if address.hasPrefix("["), let close = address.firstIndex(of: "]") {
            String(address[address.index(after: address.startIndex)..<close])
        } else {
            String(address.split(separator: ":").first ?? "")
        }
    }

    /// What loopback is called.
    static let loopback: Set<String> = ["localhost", "127.0.0.1", "::1"]

    /// Connected first, then by round trip.
    var statusRank: Double { Self.rank(connection, latencyMs: latencyMs) }

    static func rank(_ connection: ConnectionState, latencyMs: UInt32?) -> Double {
        switch connection {
        case .connected: Double(latencyMs ?? 0)
        case .connecting: 1_000_000
        case .disconnected: 2_000_000
        }
    }
}

/// A row of Setup's machine table: a machine that runs sessions, or a vault.
enum MachineLine: Hashable, Identifiable {
    case machine(MachineRow)
    case vault(VaultLine)

    var id: String {
        switch self {
        case .machine(let row): "\(row.hostId)"
        case .vault(let line): line.id
        }
    }

    var name: String {
        switch self {
        case .machine(let row): row.name
        case .vault(let line): line.name
        }
    }

    /// Connected first, then by round trip; a vault this device is not paired with last.
    var statusRank: Double {
        switch self {
        case .machine(let row): row.statusRank
        case .vault(.paired(let vault)): MachineRow.rank(vault.connection, latencyMs: nil)
        case .vault(.unpaired): 3_000_000
        }
    }

    var running: Int {
        if case .machine(let row) = self { row.running } else { -1 }
    }

    /// "owner", "member", for the You column.
    var role: String {
        switch self {
        case .machine(let row): row.role ?? ""
        case .vault(.paired(let vault)): vault.owned ? "owner" : "member"
        case .vault(.unpaired): ""
        }
    }
}

/// Setup's machines as one table, the vaults among them, each row opening its page; a vault
/// this device is not paired with offers Add. It says what it means in plain words:
/// "Connected", "Up to date"; an address goes in secondary text.
struct MachinesView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    @State private var unpaired: [UnpairedVault] = []
    @Environment(\.pages) private var pages
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    var body: some View {
        let rows = MachineRow.rows(fleet.machines, summaries: fleet.lists.machines)
        let vaults = fleet.vaults
        let lines = rows.map(MachineLine.machine)
            + (vaults.map(VaultLine.paired) + unpaired.map(VaultLine.unpaired)).map(MachineLine.vault)
        SortableTable("machines", rows: lines, sortedBy: TableSort(column: "Machine"),
                      columns: columns(backups: Self.backups(vaults))) { line in
            switch line {
            case .machine(let row): pages.open(.machine(row.hostId))
            case .vault(.paired(let vault)): pages.open(.vault(vault.hostId))
            case .vault(.unpaired(let vault)): sheet = .addMachine(vault: vault)
            }
        }
        // Asked again as machines connect, and as a vault gets paired.
        .task(id: fleet.machines.filter { $0.connection == .connected }.map(\.hostId)) {
            unpaired = await fleet.unpairedVaults()
        }
    }

    private var compact: Bool {
        #if os(iOS)
        sizeClass == .compact
        #else
        false
        #endif
    }

    /// How current each host's backup is, by host: "Up to date", "6 h behind".
    static func backups(_ vaults: [VaultStats]) -> [HostId: String] {
        var backups: [HostId: String] = [:]
        for vault in vaults {
            for host in vault.hosts {
                let behind = host.freshness.behind(now: vault.now)
                backups[host.id] = behind == 0 ? "Up to date" : "\(Freshness.span(behind)) behind"
            }
        }
        return backups
    }

    private func columns(backups: [HostId: String]) -> [SortableColumn<MachineLine>] {
        var columns: [SortableColumn<MachineLine>] = [
            SortableColumn("Machine", width: compact ? nil : 260, value: \.name) { line in
                MachineCell(line: line)
            },
            SortableColumn("Status", width: 120, value: \.statusRank) { line in
                switch line {
                case .machine(let row):
                    StatusCell(connection: row.connection, text: Self.status(row.connection),
                               help: row.latencyMs.map { "\($0) ms" })
                case .vault(.paired(let vault)):
                    StatusCell(connection: vault.connection, text: Self.status(vault.connection))
                case .vault(.unpaired):
                    Text("–").foregroundStyle(Theme.tertiary)
                }
            },
        ]
        guard !compact else {
            columns.append(SortableColumn("Running", width: 64, alignment: .trailing, value: \.running) { line in
                RunningCell(line: line)
            })
            return columns
        }
        columns += [
            SortableColumn("Running", width: 64, alignment: .trailing, value: \.running) { line in
                RunningCell(line: line)
            },
            SortableColumn("Load", width: 96, alignment: .trailing) { line in
                if case .machine(let row) = line, let cpu = row.cpu {
                    HStack(spacing: 8) {
                        UsageBar(percent: cpu, height: 5).frame(width: 48)
                        Text("\(Int(cpu.rounded()))%").monospacedDigit().foregroundStyle(Theme.secondary)
                            .frame(width: 36, alignment: .trailing)
                    }
                    .help("CPU \(Int(cpu.rounded()))%" + (row.memory.map { " · memory \(Int($0.rounded()))%" } ?? ""))
                } else {
                    Text("–").foregroundStyle(Theme.tertiary)
                }
            },
            SortableColumn("CLIs", width: 96) { line in
                if case .machine(let row) = line, let updates = row.updates {
                    if updates == 0 {
                        Text("Up to date").foregroundStyle(Theme.secondary).lineLimit(1)
                    } else {
                        Button { pages.open(.machine(row.hostId)) } label: {
                            Chip(text: updates == 1 ? "1 update" : "\(updates) updates")
                        }
                        .buttonStyle(.plain)
                        .help("Update on its page")
                    }
                } else {
                    Text("–").foregroundStyle(Theme.tertiary)
                }
            },
            SortableColumn("Accounts", width: 72, alignment: .trailing) { line in
                if case .machine(let row) = line {
                    // Signed out needs you: an amber dot, as Running marks its count with a blue one.
                    HStack(spacing: 5) {
                        if row.signedOut > 0 { Circle().fill(Theme.accent).frame(width: 6, height: 6) }
                        Text("\(row.accounts)").monospacedDigit()
                    }
                    .help(row.signedOut > 0 ? "\(row.signedOut) signed out" : "")
                } else {
                    Text("–").foregroundStyle(Theme.tertiary)
                }
            },
            SortableColumn("Backup", width: 150) { line in
                switch line {
                case .machine(let row):
                    let backup = backups[row.hostId]
                    Text(backup ?? "–").foregroundStyle(backup == nil ? Theme.tertiary : Theme.secondary).lineLimit(1)
                        .help(row.backsUpTo.map { "Backs up to \($0)" } ?? "Not backed up")
                case .vault(let vaultLine):
                    BackupCell(line: vaultLine) {
                        if case .unpaired(let vault) = vaultLine { sheet = .addMachine(vault: vault) }
                    }
                }
            },
            SortableColumn("You", value: \.role) { line in
                Text(line.role.capitalized).foregroundStyle(Theme.secondary).lineLimit(1)
            },
        ]
        return columns
    }

    /// How many of the machines that run sessions are connected: "2 of 3 machines connected".
    static func connections(_ machines: [MachineSummary]) -> String {
        let hosts = machines.filter { !$0.vault }
        let connected = hosts.filter(\.connected).count
        if hosts.isEmpty { return "No machines yet" }
        if connected < hosts.count { return "\(connected) of \(hosts.count) machines connected" }
        return hosts.count == 1 ? "1 machine connected" : "All \(hosts.count) machines connected"
    }

    /// A connection's state in a word or two: "Connected", "Connecting…", "Offline".
    static func status(_ connection: ConnectionState) -> String {
        switch connection {
        case .connected: "Connected"
        case .connecting: "Connecting…"
        case .disconnected: "Offline"
        }
    }
}

/// A row's first cell: its name, a vault marked as one, then what it is and where, the
/// address in monospace.
private struct MachineCell: View {
    let line: MachineLine

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            HStack(spacing: 6) {
                Text(line.name).fontWeight(.semibold).lineLimit(1)
                if case .vault = line { Pill(text: "vault").fixedSize() }
            }
            detail.font(Theme.meta).foregroundStyle(Theme.tertiary).lineLimit(1)
        }
    }

    @ViewBuilder private var detail: some View {
        switch line {
        case .machine(let row):
            let address = row.address.map(MachineRow.host(of:))
            switch (row.local, address) {
            case (true, let address?): Text("this Mac · \(Text(address).font(Theme.monoSmall))")
            case (true, nil): Text("this Mac")
            case (false, let address?): Text(address).font(Theme.monoSmall)
            case (false, nil): EmptyView()
            }
        case .vault(let vault):
            switch vault {
            case .paired: Text(vault.hosts == 1 ? "Backs up 1 machine" : "Backs up \(vault.hosts) machines")
            case .unpaired: Text("Not on this device yet")
            }
        }
    }
}

/// How many sessions run on a machine, a blue dot by a count above none; a dash for a vault.
private struct RunningCell: View {
    let line: MachineLine

    var body: some View {
        if case .machine(let row) = line {
            HStack(spacing: 5) {
                if row.running > 0 { Circle().fill(Theme.running).frame(width: 6, height: 6) }
                Text("\(row.running)").monospacedDigit()
                    .foregroundStyle(row.running > 0 ? Theme.text : Theme.tertiary)
            }
        } else {
            Text("–").foregroundStyle(Theme.tertiary)
        }
    }
}

/// A vault as a row of Setup's machine table: one this device is paired with,
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
        case .unpaired(let vault): vault.name
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

    /// "Up to date", or "work-mbp 6 h behind" for the host furthest behind; nil while unknown.
    var freshness: String? {
        guard case .paired(let vault) = self, !vault.hosts.isEmpty else { return nil }
        guard let host = vault.furthestBehind else { return "Up to date" }
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
    /// The latest ping's round trip, on hover.
    var help: String?

    var body: some View {
        HStack(spacing: 6) {
            ConnectionMark(state: connection)
            Text(text).foregroundStyle(Theme.secondary).lineLimit(1)
        }
        .help(help ?? connection.label)
    }
}

/// How current a vault's backup is: neutral, amber only once overdue, and why on hover; for a
/// vault this device is not paired with, who backs up to it and its Pair button.
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
                Spacer(minLength: 0)
                PillButton(title: "Add", prominent: true, action: pair)
                    .help("Sign in to \(vault.name) with your passkey to see its backups and manage its secrets")
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
                        .foregroundStyle(.ground))
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

    /// The state, with how many addresses a connection under way tries: "Connecting… (trying
    /// 3 addresses)".
    func labelCounting(addresses: Int) -> String {
        self == .connecting && addresses > 1 ? "Connecting… (trying \(addresses) addresses)" : label
    }

    /// What a message sent to a machine in this state waits for, as the machine takes it only
    /// once connected; `nil` while connected, when it is on its way.
    func holdUp(addresses: Int) -> String? {
        switch self {
        case .connected: nil
        case .connecting:
            addresses > 1 ? "Connecting to the machine (trying \(addresses) addresses)…" : "Connecting to the machine…"
        case .disconnected(let error): "The machine is offline (\(Self.explain(error))). Sends once it connects."
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
