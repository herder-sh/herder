import Foundation
import Herder
import SwiftUI

/// How current a vault's copy of a host is: caught up, or how far behind and why.
enum Freshness: Hashable {
    /// The latest batch arrived within `Freshness.tolerance` of its events; `lastEvent` is when
    /// the newest one happened.
    case caughtUp(lastEvent: Date?)
    /// The host is offline; what it did since `since` is not backed up yet.
    case offline(since: Date)
    /// The latest batch arrived `lag` seconds after its newest event: the host is sending a
    /// backlog.
    case lagging(TimeInterval)

    /// A batch at most this old when it arrives is caught up.
    static let tolerance: TimeInterval = 5 * 60

    init(online: Bool, lastSeen: Date?, lagMs: UInt64?, lastEvent: Date?, now: Date) {
        if !online, let lastSeen {
            self = .offline(since: lastSeen)
        } else if let lagMs, TimeInterval(lagMs) / 1000 > Self.tolerance {
            self = .lagging(TimeInterval(lagMs) / 1000)
        } else {
            self = .caughtUp(lastEvent: lastEvent)
        }
    }

    /// How far behind, in seconds; 0 when caught up.
    func behind(now: Date) -> TimeInterval {
        switch self {
        case .caughtUp: 0
        case .offline(let since): max(0, now.timeIntervalSince(since))
        case .lagging(let lag): lag
        }
    }

    /// "✓ caught up" or "6 h behind".
    func label(now: Date) -> String {
        if case .caughtUp = self { return "✓ caught up" }
        return "\(Self.span(behind(now: now))) behind"
    }

    /// Why a host is behind, for its "Why?"; nil when it is caught up.
    func why(host: String, now: Date) -> String? {
        switch self {
        case .caughtUp:
            return nil
        case .offline:
            return "\(host) has not reached the vault for \(Self.span(behind(now: now))), so the sessions it ran since are not backed up yet. It catches up by itself once it is online again."
        case .lagging(let lag):
            return "The last batch from \(host) was \(Self.span(lag)) old when the vault stored it: \(host) is still sending what it ran while it could not reach the vault. It is caught up once its batches arrive within minutes."
        }
    }

    /// A span as a freshness says it: "3 min", "6 h", "2 d".
    static func span(_ seconds: TimeInterval) -> String {
        switch Int(seconds) {
        case ..<60: "1 min"
        case let s where s < 3600: "\(s / 60) min"
        case let s where s < 86400: "\(s / 3600) h"
        case let s: "\(s / 86400) d"
        }
    }
}

/// A vault's status and statistics, derived from what it lists: its replicated hosts and the
/// sessions that live on them.
struct VaultStats: Hashable, Identifiable {
    struct ProjectCount: Hashable, Identifiable {
        var id: String { name }
        let name: String
        let sessions: Int
    }

    struct Host: Hashable, Identifiable {
        let id: String
        let name: String
        let online: Bool
        let sessions: Int
        /// Its projects with the most sessions, at most three.
        let topProjects: [ProjectCount]
        let freshness: Freshness
        /// When the vault stored its latest batch, once it has stored one since it started.
        var lastBatch: Date?
    }

    var id: HostId { hostId }
    let hostId: HostId
    let name: String
    let connection: ConnectionState
    let hosts: [Host]
    /// Whether this device's user owns the vault, and so may change what its hosts may do.
    let owned: Bool
    let byState: [SessionState: Int]
    let storageBytes: UInt64?
    /// When the vault was built, for the freshness it measures against.
    let now: Date

    var sessions: Int { byState.values.reduce(0, +) }

    func count(_ state: SessionState) -> Int { byState[state] ?? 0 }

    /// When the vault last stored a batch from any host.
    var lastBatch: Date? { hosts.compactMap(\.lastBatch).max() }

    /// The host furthest behind, if any is.
    var furthestBehind: Host? {
        hosts.filter { $0.freshness.behind(now: now) > 0 }.max { $0.freshness.behind(now: now) < $1.freshness.behind(now: now) }
    }

    /// "Connected · backing up 2 hosts · last batch 3 min. ago".
    @MainActor var status: String {
        let backing = hosts.count == 1 ? "backing up 1 host" : "backing up \(hosts.count) hosts"
        let batch = lastBatch.map { "last batch \(RelativeTime.text($0, now: now))" }
        return [connection.label, backing, batch].compactMap(\.self).joined(separator: " · ")
    }

    /// "245 MB".
    var size: String? {
        storageBytes.map { ByteCountFormatter.string(fromByteCount: Int64(clamping: $0), countStyle: .file) }
    }

    /// `nil` unless the machine is a vault, which replicates hosts.
    init?(machine: Machine, sessions: [SessionKey: SessionModel], now: Date = .now) {
        guard !machine.hosts.isEmpty || machine.vault != nil else { return nil }
        let entries = machine.sessions.map { head in
            let key = SessionKey(hostId: machine.hostId, sessionId: head.sessionId)
            // A copy whose host is connected here is followed on that host instead.
            var model = sessions[key] ?? head.hostId.flatMap { sessions[SessionKey(hostId: $0, sessionId: head.sessionId)] }
                ?? SessionModel(key: key)
            // Until its events arrive, the vault's listing says what state it is in.
            if !model.loaded { model.status = head.status }
            return Lists.Entry(machine: machine, head: head, model: model)
        }

        hostId = machine.hostId
        name = machine.name
        connection = machine.connection
        owned = machine.role == .owner
        storageBytes = machine.vault?.storageBytes
        byState = Dictionary(grouping: entries, by: \.model.state).mapValues(\.count)
        self.now = now
        hosts = machine.hosts.map { host in
            let own = entries.filter { $0.head.hostId == host.hostId }
            let replication = machine.vault?.hosts.first { $0.hostId == host.hostId }
            let lastEvent = replication?.lastEventAt.flatMap(Timestamp.date)
            let byProject = Dictionary(grouping: own.compactMap(\.projectId), by: { $0 }).mapValues(\.count)
            return Host(
                id: host.hostId, name: host.hostName, online: host.online,
                sessions: own.count,
                topProjects: byProject
                    .map { ProjectCount(name: Lists.projectName($0.key, machines: [machine]), sessions: $0.value) }
                    .sorted { ($1.sessions, $0.name.lowercased()) < ($0.sessions, $1.name.lowercased()) }
                    .prefix(3)
                    .map { $0 },
                freshness: Freshness(online: host.online, lastSeen: Timestamp.date(host.lastSeen),
                                     lagMs: replication?.lagMs, lastEvent: lastEvent, now: now),
                // The batch's newest event is the newest stored, and it arrived `lag` after it.
                lastBatch: replication?.lagMs.flatMap { lag in
                    lastEvent.map { $0.addingTimeInterval(TimeInterval(lag) / 1000) }
                })
        }
        .sorted { $0.name.lowercased() < $1.name.lowercased() }
    }
}

/// A vault that paired machines back up to while this device is not paired with it, so it
/// has no statistics to show.
struct UnpairedVault: Hashable, Identifiable {
    var id: String { fingerprint }
    let address: String
    let fingerprint: String
    /// The machines that back up to it, by name.
    let hosts: [String]
    /// The machine it runs on, when a machine backing up to it reaches it on loopback.
    var machine: String?

    /// What it is called: after its machine, "draugelis’s vault", else its address without the
    /// port; never the raw address.
    var name: String { machine.map(Self.name(machine:)) ?? MachineRow.host(of: address) }

    /// A vault's name after the machine it runs on: "draugelis’s vault".
    nonisolated static func name(machine: String) -> String { "\(machine)’s vault" }

    /// What `machine` is called among the others: a vault apart from the machine it runs on,
    /// "draugelis’s vault", unless its name says vault already.
    nonisolated static func name(of machine: Herder.Machine) -> String {
        let vault = !machine.hosts.isEmpty || machine.vault != nil
        return vault && !machine.name.lowercased().contains("vault") ? name(machine: machine.name) : machine.name
    }

    /// The vaults `links` name, each machine's by its name, without those whose fingerprint
    /// is in `paired`.
    static func from(_ links: [(host: String, vault: LinkedVault)], paired: Set<String>) -> [UnpairedVault] {
        Dictionary(grouping: links.filter { !paired.contains($0.vault.fingerprint) }, by: { $0.vault.fingerprint })
            .map { fingerprint, links in
                // Only the machine it runs on reaches it on loopback.
                let local = links.first { MachineRow.isLocal($0.vault.address, names: MachineRow.loopback) }?.host
                return UnpairedVault(address: links[0].vault.address, fingerprint: fingerprint,
                                     hosts: links.map { $0.host }.sorted { $0.lowercased() < $1.lowercased() },
                                     machine: local)
            }
            .sorted { $0.name.lowercased() < $1.name.lowercased() }
    }

    var backsUp: String {
        "\(ListFormatter.localizedString(byJoining: hosts)) \(hosts.count == 1 ? "backs" : "back") up here"
    }
}

extension Fleet {
    /// How long a machine gets to say where it backs up.
    static let vaultLinkTimeout: Duration = .seconds(10)

    /// The paired vaults' statistics.
    var vaults: [VaultStats] {
        liveMachines.compactMap { VaultStats(machine: $0, sessions: sessions) }
    }

    /// The vaults the connected machines back up to that this device is not paired with. Only
    /// an owner may ask a machine where it backs up; a machine that does not answer is left out.
    func unpairedVaults() async -> [UnpairedVault] {
        let asked = machines.filter { $0.connection == .connected && $0.hosts.isEmpty && $0.role == .owner }
        let client = client
        var links: [(host: String, vault: LinkedVault)] = []
        await withTaskGroup(of: (String, LinkedVault?).self) { group in
            for machine in asked {
                let (hostId, name) = (machine.hostId, machine.name)
                group.addTask {
                    let vault = try? await answered(within: Self.vaultLinkTimeout, or: "did not say where it backs up") {
                        guard case .vaultLink(_, let vault, _, _) = try await client.send(hostId: hostId, command: .getVaultLink)
                        else { return nil as LinkedVault? }
                        return vault
                    }
                    return (name, vault)
                }
            }
            for await (name, vault) in group {
                if let vault { links.append((name, vault)) }
            }
        }
        return UnpairedVault.from(links, paired: Set(machines.map(\.fingerprint)))
    }
}
