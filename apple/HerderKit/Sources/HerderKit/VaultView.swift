import Foundation
import Herder
import SwiftUI

/// A vault's status and statistics, derived from what it lists: its replicated hosts and the
/// sessions that live on them.
struct VaultStats: Hashable, Identifiable {
    /// The states in the order the breakdowns show them.
    static let order: [SessionState] = [.running, .needsYou, .waiting, .idle, .error, .moved, .archived]

    struct ProjectCount: Hashable, Identifiable {
        var id: String { name }
        let name: String
        let sessions: Int
    }

    struct Host: Hashable, Identifiable {
        let id: String
        let name: String
        let online: Bool
        /// "now" while online, else how long ago it was last seen: "5m", "2h", "3d".
        let lastSeen: String
        let byState: [SessionState: Int]
        /// Its projects with the most sessions, at most three.
        let topProjects: [ProjectCount]

        var replicatedEvents: UInt64?
        var replicationLagMs: UInt64?
        var lastReplicatedEvent: String?

        var replicationSummary: String {
            guard let replicatedEvents else { return "Replication status unavailable" }
            let lag = replicationLagMs.map { "Last batch lag: \($0) ms" } ?? "No batch received since vault restart"
            return "\(replicatedEvents) events · \(lag)"
        }

        var sessions: Int { byState.values.reduce(0, +) }
        var needsYou: Int { byState[.needsYou] ?? 0 }
    }

    var id: HostId { hostId }
    let hostId: HostId
    let name: String
    let connection: ConnectionState
    let hosts: [Host]
    let byState: [SessionState: Int]
    /// Open and draft pull requests linked to its sessions.
    let openPRs: Int
    /// Projects it knows or runs sessions in.
    let projects: Int
    let storedEvents: UInt64?
    let storageBytes: UInt64?

    var hostsOnline: Int { hosts.filter(\.online).count }
    var sessions: Int { byState.values.reduce(0, +) }

    func count(_ state: SessionState) -> Int { byState[state] ?? 0 }

    /// `nil` unless the machine is a vault, which replicates hosts.
    init?(machine: Machine, sessions: [SessionKey: SessionModel], now: Date = .now) {
        guard !machine.hosts.isEmpty || machine.vault != nil else { return nil }
        let entries = machine.sessions.map { head in
            let key = SessionKey(hostId: machine.hostId, sessionId: head.sessionId)
            var model = sessions[key] ?? SessionModel(key: key)
            // Until its events arrive, the vault's listing says what state it is in.
            if !model.loaded { model.status = head.status }
            return Lists.Entry(machine: machine, head: head, model: model)
        }
        func tally(_ entries: [Lists.Entry]) -> [SessionState: Int] {
            Dictionary(grouping: entries, by: \.model.state).mapValues(\.count)
        }

        hostId = machine.hostId
        name = machine.name
        connection = machine.connection
        storedEvents = machine.vault?.events
        storageBytes = machine.vault?.storageBytes
        byState = tally(entries)
        openPRs = entries.flatMap(\.model.prs).filter { $0.state.rank == 0 }.count
        projects = Set(machine.projects.map(\.projectId) + entries.compactMap(\.projectId)).count
        hosts = machine.hosts.map { host in
            let own = entries.filter { $0.head.hostId == host.hostId }
            let replication = machine.vault?.hosts.first { $0.hostId == host.hostId }
            let byProject = Dictionary(grouping: own.compactMap(\.projectId), by: { $0 }).mapValues(\.count)
            return Host(
                id: host.hostId, name: host.hostName, online: host.online,
                lastSeen: host.online ? "now" : Timestamp.age(Timestamp.date(host.lastSeen), now: now),
                byState: tally(own),
                topProjects: byProject
                    .map { ProjectCount(name: Lists.projectName($0.key, machines: [machine]), sessions: $0.value) }
                    .sorted { ($1.sessions, $0.name.lowercased()) < ($0.sessions, $1.name.lowercased()) }
                    .prefix(3)
                    .map { $0 },
                replicatedEvents: replication?.events,
                replicationLagMs: replication?.lagMs,
                lastReplicatedEvent: replication?.lastEventAt)
        }
        .sorted { ($1.online ? 1 : 0, $0.name.lowercased()) < ($0.online ? 1 : 0, $1.name.lowercased()) }
    }
}

extension Fleet {
    /// The paired vaults' statistics.
    var vaults: [VaultStats] {
        machines.compactMap { VaultStats(machine: $0, sessions: sessions) }
    }
}

/// Each paired vault: its connection, fleet totals, a card per replicated host, and its
/// sessions by state.
struct VaultView: View {
    let fleet: Fleet

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                let vaults = fleet.vaults
                if vaults.isEmpty {
                    Text("No vault paired.").foregroundStyle(Theme.tertiary).padding(.top, 30)
                }
                ForEach(vaults) { VaultSection(vault: $0) }
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 24)
        }
        .background(Theme.background)
        .refreshable { fleet.wake() }
    }
}

/// One vault: what `VaultView` lists, apart so it renders outside a scroll view too.
struct VaultSection: View {
    let vault: VaultStats

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            header
            totals
            if let events = vault.storedEvents, let bytes = vault.storageBytes {
                Text("\(events) replicated events · \(ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file)) stored")
                    .font(.footnote).foregroundStyle(Theme.secondary)
            }
            VStack(alignment: .leading, spacing: 10) {
                SectionHeading(title: "Hosts", count: vault.hosts.count)
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 300), spacing: 14, alignment: .top)], spacing: 14) {
                    ForEach(vault.hosts) { HostCard(host: $0) }
                }
            }
            VStack(alignment: .leading, spacing: 10) {
                SectionHeading(title: "Sessions by state", count: vault.sessions)
                Card { StateBreakdown(byState: vault.byState) }
            }
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Image(systemName: "archivebox").font(.title3)
                .foregroundStyle(vault.connection == .connected ? Theme.text : Theme.tertiary)
            VStack(alignment: .leading, spacing: 2) {
                Text(vault.name).font(.headline).foregroundStyle(Theme.text)
                HStack(spacing: 5) {
                    ConnectionMark(state: vault.connection)
                    Text(vault.connection.label).lineLimit(2)
                }
                .font(.caption)
                .foregroundStyle(Theme.secondary)
            }
        }
    }

    private var totals: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 110), spacing: 10)], spacing: 10) {
            Stat(label: "Hosts online", value: "\(vault.hostsOnline)", detail: "of \(vault.hosts.count)",
                 tint: vault.hostsOnline < vault.hosts.count ? Theme.failure : Theme.text)
            Stat(label: "Sessions", value: "\(vault.sessions)")
            Stat(label: "Running", value: "\(vault.count(.running))", tint: Theme.running)
            Stat(label: "Needs you", value: "\(vault.count(.needsYou))",
                 tint: vault.count(.needsYou) > 0 ? Theme.accent : Theme.text)
            Stat(label: "Idle", value: "\(vault.count(.idle))")
            Stat(label: "Archived", value: "\(vault.count(.archived))", tint: Theme.tertiary)
            Stat(label: "Open PRs", value: "\(vault.openPRs)")
            Stat(label: "Projects", value: "\(vault.projects)")
        }
    }
}

/// One figure with its label.
private struct Stat: View {
    let label: String
    let value: String
    var detail: String?
    var tint: Color = Theme.text

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label.uppercased()).font(.caption2.weight(.semibold)).tracking(0.5).foregroundStyle(Theme.secondary)
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Text(value).font(.title2.weight(.semibold).monospacedDigit()).foregroundStyle(tint)
                if let detail { Text(detail).font(.caption).foregroundStyle(Theme.tertiary) }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// A replicated host: whether it is online, its sessions by state, and its busiest projects.
private struct HostCard: View {
    let host: VaultStats.Host

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 12) {
                HStack(spacing: 8) {
                    Circle().fill(host.online ? Theme.success : Theme.failure).frame(width: 8, height: 8)
                    Text(host.name).font(.headline).foregroundStyle(Theme.text).lineLimit(1)
                    Spacer()
                    Text(host.online ? "Online" : host.lastSeen.isEmpty ? "Offline" : "Seen \(host.lastSeen) ago")
                        .font(.caption)
                        .foregroundStyle(host.online ? Theme.secondary : Theme.failure)
                }
                HStack(spacing: 6) {
                    Text(host.sessions == 1 ? "1 session" : "\(host.sessions) sessions").foregroundStyle(Theme.text)
                    if host.needsYou > 0 {
                        Text("· \(host.needsYou) need\(host.needsYou == 1 ? "s" : "") you").foregroundStyle(Theme.accent)
                    }
                }
                .font(.subheadline.weight(.medium))
                Text(host.replicationSummary)
                    .font(.caption).foregroundStyle(Theme.secondary)
                if let at = host.lastReplicatedEvent {
                    Text("Newest stored event: \(at)")
                        .font(.caption).foregroundStyle(Theme.tertiary)
                }
                if host.sessions > 0 {
                    StateBar(byState: host.byState)
                    FlowCounts(byState: host.byState)
                }
                if !host.topProjects.isEmpty {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(host.topProjects) { project in
                            HStack(spacing: 6) {
                                Image(systemName: "shippingbox").imageScale(.small).foregroundStyle(Theme.tertiary)
                                Text(project.name).foregroundStyle(Theme.secondary).lineLimit(1)
                                Spacer()
                                Text("\(project.sessions)").foregroundStyle(Theme.tertiary).monospacedDigit()
                            }
                        }
                    }
                    .font(.caption)
                }
            }
        }
        .opacity(host.online ? 1 : 0.8)
    }
}

/// The non-zero states as glyph and count, in order.
private struct FlowCounts: View {
    let byState: [SessionState: Int]

    var body: some View {
        HStack(spacing: 10) {
            ForEach(VaultStats.order.filter { (byState[$0] ?? 0) > 0 }, id: \.self) { state in
                HStack(spacing: 2) {
                    StatusGlyph(state: state, size: 6)
                    Text("\(byState[state] ?? 0)").monospacedDigit()
                }
                .help(state.label)
            }
        }
        .font(.caption.weight(.medium))
        .foregroundStyle(Theme.secondary)
    }
}

/// Sessions by state as one segmented bar.
private struct StateBar: View {
    let byState: [SessionState: Int]
    var height: CGFloat = 6

    var body: some View {
        let total = max(1, byState.values.reduce(0, +))
        let shown = VaultStats.order.filter { (byState[$0] ?? 0) > 0 }
        GeometryReader { geometry in
            let room = geometry.size.width - 2 * CGFloat(max(0, shown.count - 1))
            HStack(spacing: 2) {
                ForEach(shown, id: \.self) { state in
                    Rectangle().fill(state.tint)
                        .frame(width: max(2, room * CGFloat(byState[state] ?? 0) / CGFloat(total)))
                }
            }
        }
        .frame(height: height)
        .clipShape(.capsule)
    }
}

/// Every state with its count and share.
private struct StateBreakdown: View {
    let byState: [SessionState: Int]

    var body: some View {
        let total = byState.values.reduce(0, +)
        VStack(alignment: .leading, spacing: 12) {
            if total > 0 { StateBar(byState: byState, height: 8) }
            ForEach(VaultStats.order, id: \.self) { state in
                let count = byState[state] ?? 0
                HStack(spacing: 8) {
                    StatusGlyph(state: state, size: 7)
                    Text(state.label).foregroundStyle(count > 0 ? Theme.text : Theme.tertiary)
                    Spacer()
                    Text("\(count)").foregroundStyle(count > 0 ? Theme.text : Theme.tertiary).monospacedDigit()
                    Text(total > 0 ? "\(Int((100 * Double(count) / Double(total)).rounded()))%" : "–")
                        .frame(width: 40, alignment: .trailing)
                        .foregroundStyle(Theme.tertiary)
                        .monospacedDigit()
                }
                .font(.subheadline)
            }
        }
    }
}

extension SessionState {
    /// The colour its glyph uses.
    var tint: Color {
        switch self {
        case .running: Theme.running
        case .needsYou: Theme.accent
        case .waiting: Theme.waiting
        case .idle: Theme.idle
        case .done: Theme.success
        case .error: Theme.failure
        case .archived, .moved: Theme.stroke
        }
    }
}
