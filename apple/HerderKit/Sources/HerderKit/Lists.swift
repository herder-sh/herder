import Foundation
import Herder

/// A project and its sessions across machines, in task-tree order: the task trees with a live
/// session, then the ones archived whole.
struct ProjectGroup: Hashable, Identifiable {
    var id: String { projectId ?? "" }
    /// `nil` for sessions whose project is not known yet.
    let projectId: String?
    let name: String
    let machines: [String]
    /// Its task trees with a session that is not archived, whole: an archived parent stays to
    /// lead its live children, and archived children stay under their parent.
    var live: [SessionSummary]
    /// Its task trees whose every session is archived.
    var archived: [SessionSummary]
    /// Where its machines keep its clones.
    var paths: [String] = []
    /// How long since any of its sessions last did something; empty without sessions.
    var age = ""
    /// Its place among the projects by when a session in it last did something, 0 the most
    /// recent. A rank rather than the time itself, so the lists change when the order does,
    /// not on every event a session streams.
    var recency = 0

    var sessions: [SessionSummary] { live + archived }

    /// What its sidebar row and header show: its live sessions' states rolled up, as the TUI
    /// rolls up a project; `nil` without any.
    var state: SessionState? { SessionState.rollup(live.map(\.state)) }

    /// Whether the project itself matches a search: its name, id or a clone's path.
    func matches(_ query: String) -> Bool {
        let query = query.trimmingCharacters(in: .whitespaces).lowercased()
        guard !query.isEmpty else { return true }
        return ([name] + [projectId ?? ""] + paths).contains { $0.lowercased().contains(query) }
    }

    /// The projects a search finds, most recently active first, the sessions waiting for a
    /// project last. A project that matches keeps all its sessions; one that matches only
    /// through some of its sessions keeps just those.
    static func found(_ projects: [ProjectGroup], query: String) -> [ProjectGroup] {
        projects
            .compactMap { project -> ProjectGroup? in
                if project.matches(query) { return project }
                var found = project
                found.live = project.live.filter { $0.matches(query) }
                found.archived = project.archived.filter { $0.matches(query) }
                return found.sessions.isEmpty ? nil : found
            }
            .sorted { a, b in
                if (a.projectId == nil) != (b.projectId == nil) { return b.projectId == nil }
                return a.recency < b.recency
            }
    }
}

/// What the lists show, built from the machines and their sessions' folded state the way the
/// TUI builds its own (crates/herder-tui/src/app.rs, projects.rs, inbox.rs).
struct Lists: Equatable {
    var requests: [PendingRequest] = []
    /// Home's sessions: every task tree whose top is neither archived nor moved or that has a
    /// working session, newest first by when it was created, so a session keeps its place as
    /// it starts and stops working.
    var home: [SessionSummary] = []
    var projects: [ProjectGroup] = []
    var machines: [MachineSummary] = []

    init() {}

    /// `done` holds the sessions that finished a turn since this device last opened them;
    /// `archiving` the ones a machine is archiving, shown archived already.
    init(machines: [Machine], sessions: [SessionKey: SessionModel], done: Set<SessionKey> = [],
         archiving: Set<SessionKey> = [], now: Date = .now) {
        var entries: [Entry] = []
        let shadowed = Self.shadowed(machines)
        for machine in machines {
            for head in machine.sessions {
                let key = SessionKey(hostId: machine.hostId, sessionId: head.sessionId)
                if shadowed.contains(key) { continue }
                var model = sessions[key] ?? SessionModel(key: key)
                if archiving.contains(key) { model.status = .archived }
                entries.append(Entry(machine: machine, head: head, model: model, done: done.contains(key)))
            }
        }

        // Each session's children, found once: the lists are rebuilt as sessions stream, so
        // looking them up per session, over every session, would grow with the square of them.
        let children = Self.children(entries)
        projects = Self.projects(entries, machines: machines, now: now)
        let flat = entries.map { $0.summary(now: now, children: children) }
        home = Self.home(entries, children: children, now: now)

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

    /// Home's task trees, newest first by when the top session was created: the top session
    /// leads, its working children under it. Idle children are left to their parent, so they
    /// never crowd Home. A tree whose top is archived or moved is left out unless it still
    /// has a working session, which keeps its top in place to lead it.
    private static func home(_ entries: [Entry], children: [SessionKey: [Entry]], now: Date) -> [SessionSummary] {
        func isWorking(_ entry: Entry) -> Bool { [.running, .waiting, .needsYou].contains(entry.model.state) }
        func descendants(_ entry: Entry, depth: Int = 0) -> [Entry] {
            guard depth < 8 else { return [] }
            return (children[entry.key] ?? []).sorted { $0.key.sessionId < $1.key.sessionId }
                .flatMap { [$0] + descendants($0, depth: depth + 1) }
        }
        return forest(entries).filter { $0.1 == 0 }.map(\.0)
            .map { top in (top, descendants(top).filter(isWorking)) }
            .filter { top, working in ![.archived, .moved].contains(top.model.state) || !working.isEmpty }
            // Session ids are ULIDs, which sort by creation time.
            .sorted { ($0.0.key.sessionId, $0.0.key.hostId) > ($1.0.key.sessionId, $1.0.key.hostId) }
            .flatMap { top, working in
                [top.summary(now: now, children: children)] + working.map { child in
                    var summary = child.summary(now: now, children: children)
                    summary.depth = 1
                    return summary
                }
            }
    }

    /// Every project a machine lists or a listed session is in, and the sessions no project
    /// holds yet, each with its task trees that have a live session apart from those archived
    /// whole. Trees are built from all of a project's sessions before they are split, so
    /// archiving a parent or a child never takes a tree apart. An archived
    /// session no project holds is left out: it is one of a project the machine dropped, or
    /// of a repository gone from it, and would only keep that project in the list.
    private static func projects(_ entries: [Entry], machines: [Machine], now: Date) -> [ProjectGroup] {
        let shown = entries.filter { $0.projectId != nil || $0.model.state != .archived }
        var grouped: [String?: [Entry]] = Dictionary(grouping: shown, by: \.projectId)
        // Projects the machines list but no session runs in yet still show.
        for project in machines.flatMap(\.projects) where grouped[project.projectId] == nil {
            grouped[project.projectId] = []
        }
        let groups = grouped.map { projectId, members -> (ProjectGroup, Date?) in
            let name = projectId.map { projectName($0, machines: machines) } ?? "No project yet"
            let sorted = members.sorted { ($0.key.sessionId, $0.key.hostId) < ($1.key.sessionId, $1.key.hostId) }
            // A session counts only the children in its own project.
            let children = Self.children(members)
            var live: [SessionSummary] = []
            var archived: [SessionSummary] = []
            for tree in trees(forest(sorted)) {
                let summaries = tree.map { entry, depth in
                    var summary = entry.summary(now: now, children: children)
                    summary.depth = depth
                    return summary
                }
                if tree.allSatisfy({ $0.0.model.state == .archived }) {
                    archived += summaries
                } else {
                    live += summaries
                }
            }
            var machineNames = machines.filter { machine in
                projectId != nil && machine.projects.contains { $0.projectId == projectId }
            }.map(\.name)
            for entry in members where !machineNames.contains(entry.machineName) {
                machineNames.append(entry.machineName)
            }
            var paths: [String] = []
            for project in machines.flatMap(\.projects) where project.projectId == projectId {
                for path in project.paths where !paths.contains(path) { paths.append(path) }
            }
            let lastActive = members.compactMap(\.model.updatedAt).max()
            let group = ProjectGroup(
                projectId: projectId, name: name, machines: machineNames,
                live: live, archived: archived, paths: paths,
                age: Timestamp.age(lastActive, now: now))
            return (group, lastActive)
        }
        var ranked = groups.sorted { a, b in
            (a.1 ?? .distantPast, b.0.name.lowercased(), b.0.id) > (b.1 ?? .distantPast, a.0.name.lowercased(), a.0.id)
        }.map(\.0)
        for index in ranked.indices { ranked[index].recency = index }
        return ranked.sorted { a, b in
            if (a.projectId == nil) != (b.projectId == nil) { return b.projectId == nil }
            return (a.name.lowercased(), a.id) < (b.name.lowercased(), b.id)
        }
    }

    /// A project's name, the same on every device whatever order it paired its machines in: one
    /// an owner gave it on the lowest-id machine that has one, else the last part of its id.
    static func projectName(_ projectId: String, machines: [Machine]) -> String {
        let fallback = String(projectId.split(whereSeparator: { $0 == "/" || $0 == ":" }).last ?? Substring(projectId))
        return machines.sorted { $0.hostId < $1.hostId }
            .compactMap { machine in machine.projects.first { $0.projectId == projectId }?.name }
            .first { $0 != fallback } ?? fallback
    }

    /// Top-level sessions newest first, each followed by its children: the live ones oldest
    /// first, then the archived ones. A child whose parent is not listed is top-level.
    static func forest(_ entries: [Entry]) -> [(Entry, Int)] {
        let keys = Set(entries.map(\.key))
        let children = children(entries)
        func isTop(_ entry: Entry) -> Bool {
            guard let parent = entry.parentKey else { return true }
            return !keys.contains(parent)
        }
        var ordered: [(Entry, Int)] = []
        func visit(_ entry: Entry, depth: Int) {
            ordered.append((entry, depth))
            guard depth < 8 else { return }
            let kids = children[entry.key] ?? []
            for child in kids.filter({ $0.model.state != .archived }) + kids.filter({ $0.model.state == .archived }) {
                visit(child, depth: depth + 1)
            }
        }
        for top in entries.filter(isTop).reversed() {
            visit(top, depth: 0)
        }
        return ordered
    }

    /// A forest's task trees, each its top-level session and everything under it.
    static func trees(_ forest: [(Entry, Int)]) -> [[(Entry, Int)]] {
        var trees: [[(Entry, Int)]] = []
        for node in forest {
            if node.1 == 0 || trees.isEmpty { trees.append([node]) } else { trees[trees.count - 1].append(node) }
        }
        return trees
    }

    /// Each session's children among `entries`, in their order, by the parent's key.
    static func children(_ entries: [Entry]) -> [SessionKey: [Entry]] {
        var children: [SessionKey: [Entry]] = [:]
        for entry in entries {
            if let parent = entry.parentKey { children[parent, default: []].append(entry) }
        }
        return children
    }

    /// The listed copies another copy of the same session stands for, so they are neither
    /// listed nor followed. A session a vault replicates is listed once: from its own host while
    /// that host is paired here and connected; else from the vault, read-only.
    static func shadowed(_ machines: [Machine]) -> Set<SessionKey> {
        // Each session by its own host: as a connected host lists it, and as a vault does.
        var live: Set<SessionKey> = []
        var replicated: Set<SessionKey> = []
        for machine in machines {
            for head in machine.sessions {
                if let origin = head.hostId, origin != machine.hostId {
                    replicated.insert(SessionKey(hostId: origin, sessionId: head.sessionId))
                } else if machine.connection == .connected {
                    live.insert(SessionKey(hostId: machine.hostId, sessionId: head.sessionId))
                }
            }
        }
        var shadowed: Set<SessionKey> = []
        for machine in machines {
            for head in machine.sessions {
                let key = SessionKey(hostId: machine.hostId, sessionId: head.sessionId)
                if let origin = head.hostId, origin != machine.hostId {
                    if live.contains(SessionKey(hostId: origin, sessionId: head.sessionId)) { shadowed.insert(key) }
                } else if machine.connection != .connected && replicated.contains(key) {
                    shadowed.insert(key)
                }
            }
        }
        return shadowed
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
            turns: machine.hosts.isEmpty ? machine.resources.map(TurnLoad.init) : nil,
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

        /// Its parent's key; a parent is always on the same machine. The list's parent stands
        /// in until the session's own events have loaded, so its tree holds while they do.
        var parentKey: SessionKey? {
            (model.parent ?? head.parent).map { SessionKey(hostId: key.hostId, sessionId: $0) }
        }

        /// `children` holds each session's children by the parent's key; see `Lists.children`.
        func summary(now: Date, children: [SessionKey: [Entry]]) -> SessionSummary {
            let kids = children[key] ?? []
            return SessionSummary(
                // The list's title stands in until the session's own events have loaded.
                key: key, title: model.titled ?? head.title ?? model.title ?? head.task ?? "Session …\(key.sessionId.suffix(6))",
                project: projectId.map { String($0.split(whereSeparator: { $0 == "/" || $0 == ":" }).last ?? "") } ?? "",
                branch: model.branch ?? "", worktree: model.worktree ?? "", machine: machineName,
                machineOffline: host.map { !$0.online } ?? false,
                state: state, activity: model.activity,
                age: Timestamp.age(model.updatedAt, now: now), prs: model.prs,
                children: kids.count, childrenNeedYou: kids.filter(\.model.needsUser).count,
                agents: NativeAgent.listed(in: model))
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
