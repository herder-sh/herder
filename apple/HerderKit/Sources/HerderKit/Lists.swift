import Foundation
import Herder

/// A project and its sessions across machines, in task-tree order: the task trees with a live
/// session, then the ones archived whole.
struct ProjectGroup: Hashable, Identifiable {
    var id: String { projectId ?? "" }
    /// `nil` for sessions whose project is not known yet.
    let projectId: String?
    let name: String
    /// Its task trees with a session that is not archived, without their archived children: an
    /// archived parent stays only to lead its live children.
    var live: [SessionSummary]
    /// Its archived sessions, each tree of them that no live session hangs from.
    var archived: [SessionSummary]
    /// Where its machines keep its clones.
    var paths: [String] = []
    /// How long since any of its sessions last did something; empty without sessions.
    var age = ""
    /// Its place among the projects by when a session in it last did something, 0 the most
    /// recent. A rank rather than the time itself, so the lists change when the order does,
    /// not on every event a session streams.
    var recency = 0
    /// Whether none of its sessions did anything in the last 30 days, or it has none: the
    /// sidebar folds it away.
    var quiet = false

    var sessions: [SessionSummary] { live + archived }

    /// What its sidebar row and header show: its live sessions' states rolled up, as the TUI
    /// rolls up a project; `nil` without any.
    var state: SessionState? { SessionState.rollup(live.map(\.state)) }

    /// How many of its sessions wait for the user.
    var needsYou: Int { live.filter { $0.state == .needsYou }.count }

    /// How many of its sessions are running a turn.
    var working: Int { live.filter { $0.state == .running }.count }

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

/// A chat bot's conversations: the task trees its people started, on the machine that holds it.
struct BotGroup: Hashable, Identifiable {
    var id: String { "\(hostId)/\(botId)" }
    /// The machine the bot runs on: for a hub, the host its sessions run on.
    let hostId: HostId
    let botId: BotId
    let kind: BotKind
    /// Its task trees with a live session, then those archived whole, as a project's.
    var live: [SessionSummary]
    var archived: [SessionSummary]
    /// How many of its sessions have an approval or a question waiting for the user.
    var waiting = 0

    var sessions: [SessionSummary] { live + archived }
}

/// What the lists show, built from the machines and their sessions' folded state the way the
/// TUI builds its own (crates/herder-tui/src/app.rs, projects.rs, inbox.rs).
struct Lists: Equatable {
    var requests: [PendingRequest] = []
    var projects: [ProjectGroup] = []
    /// The chats, sessions about no project, in task-tree order: the trees with a live
    /// session, then the archived ones.
    var chats: [SessionSummary] = []
    var archivedChats: [SessionSummary] = []
    /// The bots' conversations, a group a bot, kept out of the projects, the chats and so the
    /// Board; a tree waiting for the user is on the Board too (`board`).
    var bots: [BotGroup] = []
    /// The sessions with an approval or a question waiting for the user.
    var waiting: Set<SessionKey> = []
    var machines: [MachineSummary] = []
    /// Each session's children: a view can walk one session's tree without reading every
    /// session, and so redrawing on every token any of them streams.
    var children: [SessionKey: [SessionKey]] = [:]
    /// The models each machine's sessions run, by provider, newest session first: the ones the
    /// model menu offers as used there.
    var usedModels: [HostId: [Provider: [String]]] = [:]

    init() {}

    /// The project a new session starts in from where the user is: the project pane's only;
    /// `nil` anywhere else, which leaves the choice to the picker.
    func project(for item: SidebarItem) -> String? {
        guard case .project(let id) = item else { return nil }
        return projects.first { $0.id == id }?.projectId
    }

    /// `done` holds the sessions that finished a turn since this device last opened them;
    /// `archiving` the ones a machine is archiving, shown archived already.
    /// `copies` says which copy of each session is listed, when the caller keeps that; see
    /// `copies(_:keeping:)`.
    init(machines: [Machine], sessions: [SessionKey: SessionModel], done: Set<SessionKey> = [],
         archiving: Set<SessionKey> = [], copies: SessionCopies? = nil, now: Date = .now) {
        var entries: [Entry] = []
        let shadowed = (copies ?? Self.copies(machines)).shadowed
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
        self.children = children.mapValues { $0.map(\.key) }
        for entry in entries.sorted(by: { ($0.model.updatedAt ?? .distantPast) > ($1.model.updatedAt ?? .distantPast) }) {
            guard let provider = entry.model.provider, let model = entry.model.model else { continue }
            var used = usedModels[entry.key.hostId, default: [:]][provider, default: []]
            if !used.contains(model) { used.append(model) }
            usedModels[entry.key.hostId, default: [:]][provider] = used
        }
        let origins = Self.origins(entries)
        let own = entries.filter { origins[$0.key] == nil }
        projects = Self.projects(own.filter { !$0.head.chat }, machines: machines, now: now)
        (chats, archivedChats) = Self.split(own.filter(\.head.chat), now: now)
        let flat = entries.map { $0.summary(now: now, children: children) }

        // One per session, as its view pins it: the questions asked together page in one card.
        requests = zip(entries, flat).compactMap { entry, summary in
            entry.model.pinned(summary, userOnly: true, now: now)
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
        waiting = Set(requests.map(\.session.key))
        bots = Self.bots(entries.filter { origins[$0.key] != nil }, origins: origins, waiting: waiting, now: now)

        self.machines = machines.map { Self.summary($0, entries: entries, now: now) }
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
            let (live, archived) = split(members, now: now)
            var paths: [String] = []
            for project in machines.flatMap(\.projects) where project.projectId == projectId {
                for path in project.paths where !paths.contains(path) { paths.append(path) }
            }
            let lastActive = members.compactMap(\.model.updatedAt).max()
            let group = ProjectGroup(
                projectId: projectId, name: name,
                live: live, archived: archived, paths: paths,
                age: Timestamp.age(lastActive, now: now),
                quiet: lastActive.map { now.timeIntervalSince($0) > 30 * 24 * 3600 } ?? true)
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

    /// `members`' task trees with a live session, then the archived sessions: a tree archived
    /// whole, and an archived child no live session hangs from, as a tree of its own. An
    /// archived parent stays to lead its live children. A session counts only the children
    /// among `members`.
    private static func split(_ members: [Entry], now: Date) -> (live: [SessionSummary], archived: [SessionSummary]) {
        let sorted = members.sorted { ($0.key.sessionId, $0.key.hostId) < ($1.key.sessionId, $1.key.hostId) }
        let children = Self.children(members)
        var live: [SessionSummary] = []
        var archived: [SessionSummary] = []
        // The sessions that are live or lead a live one: every live session and its ancestors.
        let byKey = Dictionary(members.map { ($0.key, $0) }, uniquingKeysWith: { first, _ in first })
        var leading: Set<SessionKey> = []
        for entry in members where entry.model.state != .archived {
            var key: SessionKey? = entry.key
            while let current = key, leading.insert(current).inserted { key = byKey[current]?.parentKey }
        }
        let nodes = forest(sorted)
        let leads = nodes.map { leading.contains($0.0.key) }
        // The depth an archived subtree hangs from, so it is listed as a tree of its own.
        var root = 0
        for (index, (entry, depth)) in nodes.enumerated() {
            var summary = entry.summary(now: now, children: children)
            if leads[index] {
                summary.depth = depth
                live.append(summary)
            } else {
                if index == 0 || leads[index - 1] || depth <= root { root = depth }
                summary.depth = depth - root
                archived.append(summary)
            }
        }
        return (live, archived)
    }

    /// The bot that started each session of a bot's task tree, with the host that holds the bot:
    /// a child has the origin of its tree's top.
    static func origins(_ entries: [Entry]) -> [SessionKey: (hostId: HostId, origin: SessionOrigin)] {
        let byKey = Dictionary(entries.map { ($0.key, $0) }, uniquingKeysWith: { first, _ in first })
        var origins: [SessionKey: (hostId: HostId, origin: SessionOrigin)] = [:]
        for entry in entries {
            var top = entry
            var seen: Set<SessionKey> = [entry.key]
            while let parent = top.parentKey.flatMap({ byKey[$0] }), seen.insert(parent.key).inserted { top = parent }
            if let origin = top.head.origin {
                origins[entry.key] = (top.head.hostId ?? top.machine.hostId, origin)
            }
        }
        return origins
    }

    /// The bots' groups of `members`, ordered by bot then machine, each split as a project is.
    private static func bots(_ members: [Entry], origins: [SessionKey: (hostId: HostId, origin: SessionOrigin)],
                             waiting: Set<SessionKey>, now: Date) -> [BotGroup] {
        let grouped = Dictionary(grouping: members) { entry -> String in
            let found = origins[entry.key]
            return "\(found?.origin.botId ?? "")\u{0}\(found?.hostId ?? "")"
        }
        return grouped.values.compactMap { members -> BotGroup? in
            guard let found = members.first.flatMap({ origins[$0.key] }) else { return nil }
            let (live, archived) = split(members, now: now)
            return BotGroup(hostId: found.hostId, botId: found.origin.botId, kind: found.origin.kind,
                            live: live, archived: archived,
                            waiting: members.filter { waiting.contains($0.key) }.count)
        }
        .sorted { ($0.botId, $0.hostId) < ($1.botId, $1.hostId) }
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

    /// Each session's children among `entries`, in their order, by the parent's key.
    static func children(_ entries: [Entry]) -> [SessionKey: [Entry]] {
        var children: [SessionKey: [Entry]] = [:]
        for entry in entries {
            if let parent = entry.parentKey { children[parent, default: []].append(entry) }
        }
        return children
    }

    /// The listed copies another copy of the same session stands for, so they are neither
    /// listed nor followed; see `copies`.
    static func shadowed(_ machines: [Machine]) -> Set<SessionKey> { copies(machines).shadowed }

    /// Which copy of each session is listed, where its own host and the hubs that replicate
    /// it each list one: its own host's while that host is connected; else a connected hub's,
    /// read-only; and with none of them connected the copy in `previous`, an earlier answer's
    /// `shown`, so that losing the network moves no session from one copy to another.
    static func copies(_ machines: [Machine], keeping previous: [SessionKey: SessionKey] = [:]) -> SessionCopies {
        var copies = SessionCopies()
        // Without a hub's copy of another host's session every session is listed once.
        guard machines.contains(where: { machine in
            machine.sessions.contains { $0.hostId.map { $0 != machine.hostId } ?? false }
        }) else { return copies }
        struct Copy {
            let key: SessionKey
            let own: Bool
            let connected: Bool
        }
        // Each session's copies, by the session on its own host.
        var groups: [SessionKey: [Copy]] = [:]
        for machine in machines {
            let connected = machine.connection == .connected
            for head in machine.sessions {
                let origin = SessionKey(hostId: head.hostId ?? machine.hostId, sessionId: head.sessionId)
                groups[origin, default: []].append(Copy(
                    key: SessionKey(hostId: machine.hostId, sessionId: head.sessionId),
                    own: origin.hostId == machine.hostId, connected: connected))
            }
        }
        for (origin, group) in groups where group.count > 1 {
            let kept = previous[origin]
            let hubs = group.filter { !$0.own && $0.connected }
            let own = group.first { $0.own && $0.connected }
            let hub = hubs.first { $0.key == kept } ?? hubs.first
            let before = group.first { $0.key == kept }
            let shown = own ?? hub ?? before ?? group.first { !$0.own } ?? group[0]
            copies.shown[origin] = shown.key
            for copy in group {
                copies.others[copy.key] = group.map(\.key).filter { $0 != copy.key }
                if copy.key != shown.key {
                    copies.shadowed.insert(copy.key)
                    copies.standIns[copy.key] = shown.key
                }
            }
        }
        return copies
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
            pinned: machine.failover.pin,
            hub: !machine.hosts.isEmpty || machine.hub != nil,
            openWork: own.filter { [.running, .waiting, .needsYou].contains($0.model.state) }.count)
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

        /// For a hub, the host the session runs on.
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
                // The list's title stands in until the session's own events have loaded; the
                // prompt this device sent, before the machine has journaled anything.
                key: key, title: model.titled ?? head.title ?? model.title ?? head.task
                    ?? model.outbox.first.map { firstLine($0.text) } ?? "Session …\(key.sessionId.suffix(6))",
                project: projectId.map { String($0.split(whereSeparator: { $0 == "/" || $0 == ":" }).last ?? "") } ?? "",
                projectId: projectId, branch: model.branch ?? "", worktree: model.worktree ?? "", machine: machineName,
                machineOffline: host.map { !$0.online } ?? false, fromHub: host != nil,
                state: state, activity: model.activity,
                age: Timestamp.age(model.updatedAt, now: now), updatedAt: model.updatedAt, prs: model.prs,
                children: kids.count, childrenNeedYou: kids.filter(\.model.needsUser).count,
                agents: NativeAgent.listed(in: model), threads: head.threads, origin: head.origin)
        }
    }
}

/// Which copy of each session is listed, of the sessions listed more than once: by their own
/// host and by the hubs that replicate it. See `Lists.copies`.
struct SessionCopies: Equatable {
    /// The copy listed, by the session on its own host.
    var shown: [SessionKey: SessionKey] = [:]
    /// The copies another stands for: neither listed nor followed.
    var shadowed: Set<SessionKey> = []
    /// The copy listed in place of each shadowed one.
    var standIns: [SessionKey: SessionKey] = [:]
    /// Each copy's other copies.
    var others: [SessionKey: [SessionKey]] = [:]
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

/// What the lists show now, observed list by list: a view that reads the machines redraws when
/// they change, not when a session's summary does, as a running turn's does with every round.
/// Read `all` for several at once; anything else of `Lists` reads through it.
@MainActor @Observable @dynamicMemberLookup
final class ShownLists {
    private(set) var requests: [PendingRequest] = []
    private(set) var projects: [ProjectGroup] = []
    private(set) var chats: [SessionSummary] = []
    private(set) var archivedChats: [SessionSummary] = []
    private(set) var bots: [BotGroup] = []
    private(set) var waiting: Set<SessionKey> = []
    private(set) var machines: [MachineSummary] = []
    private(set) var children: [SessionKey: [SessionKey]] = [:]
    private(set) var usedModels: [HostId: [Provider: [String]]] = [:]

    /// Every list; a view that reads it redraws when any of them changes.
    var all: Lists {
        var lists = Lists()
        lists.requests = requests
        lists.projects = projects
        lists.chats = chats
        lists.archivedChats = archivedChats
        lists.bots = bots
        lists.waiting = waiting
        lists.machines = machines
        lists.children = children
        lists.usedModels = usedModels
        return lists
    }

    subscript<T>(dynamicMember path: KeyPath<Lists, T>) -> T { all[keyPath: path] }

    func board(matching query: String = "") -> [TaskTree] { all.board(matching: query) }

    func archivedToday(matching query: String = "", now: Date = .now, calendar: Calendar = .current) -> [TaskTree] {
        all.archivedToday(matching: query, now: now, calendar: calendar)
    }

    func bots(_ filter: String?) -> [BotGroup] { bots.filter { filter == nil || $0.id == filter } }

    func project(for item: SidebarItem) -> String? {
        guard case .project(let id) = item else { return nil }
        return projects.first { $0.id == id }?.projectId
    }

    /// Takes `lists`, setting only the lists that changed.
    func show(_ lists: Lists) {
        if lists.requests != requests { requests = lists.requests }
        if lists.projects != projects { projects = lists.projects }
        if lists.chats != chats { chats = lists.chats }
        if lists.archivedChats != archivedChats { archivedChats = lists.archivedChats }
        if lists.bots != bots { bots = lists.bots }
        if lists.waiting != waiting { waiting = lists.waiting }
        if lists.machines != machines { machines = lists.machines }
        if lists.children != children { children = lists.children }
        if lists.usedModels != usedModels { usedModels = lists.usedModels }
    }
}
