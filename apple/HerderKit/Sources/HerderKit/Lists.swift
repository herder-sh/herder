import Foundation
import Herder

/// A project and its sessions across machines, in task-tree order.
struct ProjectGroup: Hashable, Identifiable {
    var id: String { projectId ?? "" }
    /// `nil` for sessions whose project is not known yet.
    let projectId: String?
    let name: String
    let machines: [String]
    let sessions: [SessionSummary]
}

/// What the lists show, built from the machines and their sessions' folded state the way the
/// TUI builds its own (crates/herder-tui/src/app.rs, projects.rs, inbox.rs).
struct Lists {
    var requests: [PendingRequest] = []
    /// Running, waiting and needing-you sessions without a request card, newest activity first.
    var active: [SessionSummary] = []
    /// Idle and failed sessions, newest activity first.
    var recent: [SessionSummary] = []
    var projects: [ProjectGroup] = []
    var machines: [MachineSummary] = []

    init(machines: [Machine], sessions: [SessionKey: SessionModel], now: Date = .now) {
        var entries: [Entry] = []
        for machine in machines {
            for head in machine.sessions {
                let key = SessionKey(hostId: machine.hostId, sessionId: head.sessionId)
                entries.append(Entry(machine: machine, head: head, model: sessions[key] ?? SessionModel(key: key)))
            }
        }

        projects = Self.projects(entries, machines: machines, now: now)
        let flat = entries.map { $0.summary(now: now, children: entries) }
        let byActivity = zip(entries, flat)
            .sorted { ($0.0.model.updatedAt ?? .distantPast) > ($1.0.model.updatedAt ?? .distantPast) }
            .map(\.1)
        // A session whose request is on a card above is not listed again.
        let asking = Set(entries.filter { !$0.model.forUser.isEmpty }.map(\.key))
        active = byActivity.filter { [.running, .waiting, .needsYou].contains($0.state) && !asking.contains($0.key) }
        recent = byActivity.filter { [.idle, .error].contains($0.state) }

        requests = zip(entries, flat).flatMap { entry, summary in
            entry.model.forUser.map { pending in
                let kind: PendingRequest.Kind = switch pending.kind {
                case .approval(let summary): .approval(summary: summary)
                case .question(let text, let choices): .question(text: text, choices: choices)
                }
                return PendingRequest(
                    requestId: pending.id, session: summary, kind: kind, since: pending.since,
                    age: Timestamp.age(pending.since, now: now), reason: pending.reason?.text,
                    note: pending.note)
            }
        }
        // Newest first; ties by machine, session, approvals before questions, then id.
        requests.sort { a, b in
            if a.since != b.since { return a.since > b.since }
            if a.session.key.hostId != b.session.key.hostId { return a.session.key.hostId < b.session.key.hostId }
            if a.session.key.sessionId != b.session.key.sessionId {
                return a.session.key.sessionId < b.session.key.sessionId
            }
            if a.isQuestion != b.isQuestion { return !a.isQuestion }
            return a.requestId < b.requestId
        }

        self.machines = machines.map { Self.summary($0, entries: entries, now: now) }
    }

    private static func projects(_ entries: [Entry], machines: [Machine], now: Date) -> [ProjectGroup] {
        let grouped = Dictionary(grouping: entries, by: \.projectId)
        return grouped.map { projectId, members in
            let name = projectId.map { id in
                machines.lazy.flatMap(\.projects).first { $0.projectId == id }?.name
                    ?? String(id.split(whereSeparator: { $0 == "/" || $0 == ":" }).last ?? Substring(id))
            } ?? "No project yet"
            let ordered = forest(members.sorted { ($0.key.sessionId, $0.key.hostId) < ($1.key.sessionId, $1.key.hostId) })
            var machineNames: [String] = []
            for entry in members where !machineNames.contains(entry.machineName) {
                machineNames.append(entry.machineName)
            }
            return ProjectGroup(
                projectId: projectId, name: name, machines: machineNames,
                sessions: ordered.map { entry, depth in
                    var summary = entry.summary(now: now, children: members)
                    summary.depth = depth
                    return summary
                })
        }
        .sorted { a, b in
            if (a.projectId == nil) != (b.projectId == nil) { return b.projectId == nil }
            return (a.name.lowercased(), a.id) < (b.name.lowercased(), b.id)
        }
    }

    /// Top-level sessions newest first, each followed by its children oldest first. A child
    /// whose parent is not listed is top-level.
    static func forest(_ entries: [Entry]) -> [(Entry, Int)] {
        let keys = Set(entries.map(\.key))
        func isTop(_ entry: Entry) -> Bool {
            guard let parent = entry.model.parent else { return true }
            return !keys.contains(SessionKey(hostId: entry.key.hostId, sessionId: parent))
        }
        var ordered: [(Entry, Int)] = []
        func visit(_ entry: Entry, depth: Int) {
            ordered.append((entry, depth))
            guard depth < 8 else { return }
            for child in entries where child.isChild(of: entry) {
                visit(child, depth: depth + 1)
            }
        }
        for top in entries.filter(isTop).reversed() {
            visit(top, depth: 0)
        }
        return ordered
    }

    private static func summary(_ machine: Machine, entries: [Entry], now: Date) -> MachineSummary {
        let own = entries.filter { $0.key.hostId == machine.hostId }
        let memory = machine.resources.flatMap { resources -> Double? in
            guard resources.memoryTotalBytes > 0 else { return nil }
            return 100 * (1 - Double(resources.memoryAvailableBytes) / Double(resources.memoryTotalBytes))
        }
        return MachineSummary(
            hostId: machine.hostId, name: machine.name, connection: machine.connection, role: machine.role,
            cpu: machine.resources?.cpuPercent, memory: memory,
            running: own.filter { $0.model.state == .running }.count,
            sessions: machine.sessions.count,
            accounts: machine.accounts.map { account in
                AccountSummary(
                    accountId: account.accountId, label: account.label, provider: account.provider,
                    sessions: own.filter { $0.model.accountId == account.accountId && $0.model.state != .archived }.count,
                    usage: account.usage.map { window in
                        UsageWindowSummary(
                            label: usageLabel(window.window), percent: window.usedPercent,
                            resets: Timestamp.until(window.resetsAt.flatMap(Timestamp.date), now: now))
                    })
            },
            hosts: machine.hosts.map { host in
                FleetHostSummary(
                    name: host.hostName, online: host.online,
                    lastSeen: Timestamp.age(Timestamp.date(host.lastSeen), now: now),
                    sessions: machine.sessions.filter { $0.hostId == host.hostId }.count)
            },
            pinned: machine.failover.pin)
    }

    /// A usage window's name as the TUI labels it (crates/herder-tui/src/account_screen.rs).
    static func usageLabel(_ window: String) -> String {
        switch window {
        case "five_hour": return "Session"
        case "seven_day", "weekly": return "Weekly"
        case "daily": return "Daily"
        default:
            if window.hasPrefix("seven_day_") {
                return "Weekly · " + window.dropFirst("seven_day_".count).capitalized
            }
            if window.hasPrefix("limit.") {
                return usageLabel(String(window.dropFirst("limit.".count))) + " · limit"
            }
            let words = window.replacingOccurrences(of: "_", with: " ")
            return words.prefix(1).uppercased() + words.dropFirst()
        }
    }

    /// One listed session with what the lists need about it.
    struct Entry {
        let machine: Machine
        let head: SessionHead
        let model: SessionModel

        var key: SessionKey { model.key }

        /// The head's project, else the local project of its repository once known.
        var projectId: String? {
            head.projectId ?? model.repo.map { "\(head.hostId ?? machine.hostId):\($0)" }
        }

        /// For a vault, the host the session runs on.
        var host: FleetHost? {
            head.hostId.flatMap { id in machine.hosts.first { $0.hostId == id } }
        }

        var machineName: String { host?.hostName ?? machine.name }

        func isChild(of parent: Entry) -> Bool {
            key.hostId == parent.key.hostId && model.parent == parent.key.sessionId
        }

        func summary(now: Date, children: [Entry]) -> SessionSummary {
            let kids = children.filter { $0.isChild(of: self) }
            return SessionSummary(
                key: key, title: model.title ?? head.task ?? "Session …\(key.sessionId.suffix(6))",
                project: projectId.map { String($0.split(whereSeparator: { $0 == "/" || $0 == ":" }).last ?? "") } ?? "",
                branch: model.branch ?? "", machine: machineName,
                machineOffline: host.map { !$0.online } ?? false,
                state: model.state, activity: model.activity,
                age: Timestamp.age(model.updatedAt, now: now), prs: model.prs,
                children: kids.count, childrenNeedYou: kids.filter(\.model.needsUser).count)
        }
    }
}

extension PendingRequest {
    var isQuestion: Bool {
        if case .question = kind { true } else { false }
    }
}

extension EscalationReason {
    /// Why a request came to the user, as the TUI says it.
    var text: String {
        switch self {
        case .markedByPrimary: "The primary session left it to you"
        case .exceedsAuthority: "Beyond what the primary session may decide"
        case .timeout: "The primary session did not answer in time"
        }
    }
}
