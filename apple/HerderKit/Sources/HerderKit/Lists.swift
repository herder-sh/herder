import Foundation
import Herder

/// A project and its sessions across machines, in task-tree order: the live ones, then the
/// archived ones, each group a task tree of its own.
struct ProjectGroup: Hashable, Identifiable {
    var id: String { projectId ?? "" }
    /// `nil` for sessions whose project is not known yet.
    let projectId: String?
    let name: String
    let machines: [String]
    let live: [SessionSummary]
    let archived: [SessionSummary]

    var sessions: [SessionSummary] { live + archived }

    /// What its sidebar row and header show: its live sessions' states rolled up, as the TUI
    /// rolls up a project; `nil` without any.
    var state: SessionState? { SessionState.rollup(live.map(\.state)) }
}

/// What the lists show, built from the machines and their sessions' folded state the way the
/// TUI builds its own (crates/herder-tui/src/app.rs, projects.rs, inbox.rs).
struct Lists {
    var requests: [PendingRequest] = []
    /// Running, waiting and needing-you sessions without a request card, newest first by when they
    /// were created, so the list holds still while they work.
    var active: [SessionSummary] = []
    /// Idle and failed sessions, newest activity first.
    var recent: [SessionSummary] = []
    var projects: [ProjectGroup] = []
    var machines: [MachineSummary] = []

    /// `done` holds the sessions that finished a turn since this device last opened them.
    init(machines: [Machine], sessions: [SessionKey: SessionModel], done: Set<SessionKey> = [], now: Date = .now) {
        var entries: [Entry] = []
        for machine in machines {
            for head in machine.sessions {
                let key = SessionKey(hostId: machine.hostId, sessionId: head.sessionId)
                entries.append(Entry(machine: machine, head: head, model: sessions[key] ?? SessionModel(key: key),
                                     done: done.contains(key)))
            }
        }

        projects = Self.projects(entries, machines: machines, now: now)
        let flat = entries.map { $0.summary(now: now, children: entries) }
        (active, recent) = Self.home(entries, now: now)

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

    /// Home's task trees. A tree is active while its top session or a child is: the top session
    /// leads, its active children under it. Active trees stay in the order they were created,
    /// newest first, so the list holds still while they work; recent ones are newest activity
    /// first. Idle children are left to their parent, so they never crowd Home.
    private static func home(_ entries: [Entry], now: Date) -> (active: [SessionSummary], recent: [SessionSummary]) {
        // A session whose request is on a card above is not listed again.
        let asking = Set(entries.filter { !$0.model.forUser.isEmpty }.map(\.key))
        func isActive(_ entry: Entry) -> Bool {
            [.running, .waiting, .needsYou].contains(entry.model.state) && !asking.contains(entry.key)
        }
        func descendants(_ entry: Entry, depth: Int = 0) -> [Entry] {
            guard depth < 8 else { return [] }
            return entries.filter { $0.isChild(of: entry) }.sorted { $0.key.sessionId < $1.key.sessionId }
                .flatMap { [$0] + descendants($0, depth: depth + 1) }
        }
        let trees = forest(entries).filter { $0.1 == 0 }
            .map { top, _ in
                let children = descendants(top)
                return (top, children, latest: ([top] + children).compactMap(\.model.updatedAt).max() ?? .distantPast)
            }
            .sorted { $0.latest > $1.latest }
        var groups: [(top: SessionKey, sessions: [SessionSummary])] = []
        var recent: [SessionSummary] = []
        for (top, children, _) in trees {
            let busy = children.filter(isActive)
            if isActive(top) || !busy.isEmpty {
                groups.append((top.key, [top.summary(now: now, children: entries)] + busy.map { child in
                    var summary = child.summary(now: now, children: entries)
                    summary.depth = 1
                    return summary
                }))
            } else if [.idle, .error].contains(top.model.state) {
                recent.append(top.summary(now: now, children: entries))
            }
        }
        // Session ids are ULIDs, which sort by creation time.
        let active = groups.sorted { $0.top.sessionId > $1.top.sessionId }.flatMap(\.sessions)
        return (active, recent)
    }

    /// Every project a machine lists or a listed session is in, and the sessions no project
    /// holds yet, each with its live sessions apart from its archived ones. An archived
    /// session no project holds is left out: it is one of a project the machine dropped, or
    /// of a repository gone from it, and would only keep that project in the list.
    private static func projects(_ entries: [Entry], machines: [Machine], now: Date) -> [ProjectGroup] {
        let shown = entries.filter { $0.projectId != nil || $0.model.state != .archived }
        var grouped: [String?: [Entry]] = Dictionary(grouping: shown, by: \.projectId)
        // Projects the machines list but no session runs in yet still show.
        for project in machines.flatMap(\.projects) where grouped[project.projectId] == nil {
            grouped[project.projectId] = []
        }
        return grouped.map { projectId, members in
            let name = projectId.map { projectName($0, machines: machines) } ?? "No project yet"
            let sorted = members.sorted { ($0.key.sessionId, $0.key.hostId) < ($1.key.sessionId, $1.key.hostId) }
            // A child whose parent is in the other group leads a tree of its own in its group.
            func tree(archived: Bool) -> [SessionSummary] {
                forest(sorted.filter { ($0.model.state == .archived) == archived }).map { entry, depth in
                    var summary = entry.summary(now: now, children: members)
                    summary.depth = depth
                    return summary
                }
            }
            var machineNames = machines.filter { machine in
                projectId != nil && machine.projects.contains { $0.projectId == projectId }
            }.map(\.name)
            for entry in members where !machineNames.contains(entry.machineName) {
                machineNames.append(entry.machineName)
            }
            return ProjectGroup(
                projectId: projectId, name: name, machines: machineNames,
                live: tree(archived: false), archived: tree(archived: true))
        }
        .sorted { a, b in
            if (a.projectId == nil) != (b.projectId == nil) { return b.projectId == nil }
            return (a.name.lowercased(), a.id) < (b.name.lowercased(), b.id)
        }
    }

    /// A project's name as its machine knows it, else the last part of its id.
    static func projectName(_ projectId: String, machines: [Machine]) -> String {
        machines.lazy.flatMap(\.projects).first { $0.projectId == projectId }?.name
            ?? String(projectId.split(whereSeparator: { $0 == "/" || $0 == ":" }).last ?? Substring(projectId))
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
        /// Whether it finished a turn since this device last opened it.
        var done = false

        var key: SessionKey { model.key }

        /// Its state, done while idle and unseen.
        var state: SessionState { model.state == .idle && done ? .done : model.state }

        /// The project its machine lists it in; `nil` until the machine's project discovery
        /// has seen its repository, and for good once the repository's project is removed.
        var projectId: String? { head.projectId }

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
                // The list's title stands in until the session's own events have loaded.
                key: key, title: model.titled ?? head.title ?? model.title ?? head.task ?? "Session …\(key.sessionId.suffix(6))",
                project: projectId.map { String($0.split(whereSeparator: { $0 == "/" || $0 == ":" }).last ?? "") } ?? "",
                branch: model.branch ?? "", worktree: model.worktree ?? "", machine: machineName,
                machineOffline: host.map { !$0.online } ?? false,
                state: state, activity: model.activity,
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

extension SessionSummary {
    /// Whether the session matches a search: its name, branch, worktree, project, machine, or
    /// a pull request's number or title.
    func matches(_ query: String) -> Bool {
        let query = query.trimmingCharacters(in: .whitespaces).lowercased()
        guard !query.isEmpty else { return true }
        let number = query.hasPrefix("#") ? String(query.dropFirst()) : query
        return [title, branch, worktree, project, machine].contains { $0.lowercased().contains(query) }
            || prs.contains { String($0.number) == number || $0.title.lowercased().contains(query) }
    }
}
