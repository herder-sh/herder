import Herder
import SwiftUI

/// A task tree: its top session, and under it the rest of the tree.
struct TaskTree: Hashable, Identifiable {
    var id: SessionKey { lead.key }
    let lead: SessionSummary
    /// The rest of the tree, in tree order.
    let children: [SessionSummary]
    /// Its project's name, as the Board names it.
    var project = ""

    /// What is listed under the top: the live sessions in tree order, then the archived.
    var listed: [SessionSummary] { live + archived }
    var live: [SessionSummary] { children.filter { $0.state != .archived } }
    var archived: [SessionSummary] { children.filter { $0.state == .archived } }

    /// How many agents work under the top: its provider's own and its herder children.
    var agentCount: Int { lead.agents.count + children.count }

    /// Where its agents stand together, as a project rolls up its sessions: a provider's agent
    /// still running counts as running; `nil` when none is live.
    var agentsState: SessionState? {
        let native: [SessionState] = lead.agents.contains { $0.outcome == .running } ? [.running] : []
        return SessionState.rollup(live.map(\.state) + native)
    }

    /// When any of its sessions last did something; `nil` until their events have loaded.
    var lastActive: Date? { ([lead] + children).compactMap(\.updatedAt).max() }

    /// When its top session was created, read from its id.
    var created: Date? { ULID.date(lead.key.sessionId) }

    /// Where the task stands, from its sessions alone: waiting for you while one of them waits
    /// on an answer or stopped on a failed turn, working while one works, else idle.
    var state: TaskState {
        let states = ([lead] + live).map { TaskState($0.state) }
        if states.contains(.waitingForYou) { return .waitingForYou }
        if states.contains(.working) || lead.agents.contains(where: { $0.outcome == .running }) { return .working }
        return .idle
    }

    /// What its row says: what the session waiting for you waits for, else what the top is doing.
    var activity: String {
        guard state == .waitingForYou else { return lead.activity }
        return ([lead] + live).first { TaskState($0.state) == .waitingForYou }?.activity ?? lead.activity
    }

    /// Whether a search finds the tree: its top or any session under it matches.
    func matches(_ query: String) -> Bool {
        ([lead] + children).contains { $0.matches(query) }
    }

    /// The trees of `sessions` listed in tree order: each top (depth 0), then the rest of its tree.
    static func trees(_ sessions: [SessionSummary]) -> [TaskTree] {
        var trees: [[SessionSummary]] = []
        for session in sessions {
            if session.depth == 0 || trees.isEmpty { trees.append([session]) } else { trees[trees.count - 1].append(session) }
        }
        return trees.map { TaskTree(lead: $0[0], children: Array($0.dropFirst())) }
    }
}

/// Where a task stands on the Board. Only these three, all read from its sessions: a pull
/// request, its checks or its review never change it.
enum TaskState: Hashable {
    case working, waitingForYou, idle

    init(_ state: SessionState) {
        switch state {
        case .running, .waiting: self = .working
        case .needsYou, .error: self = .waitingForYou
        case .idle, .done, .archived, .moved: self = .idle
        }
    }

    var label: String {
        switch self {
        case .working: "Working"
        case .waitingForYou: "Waiting for you"
        case .idle: "Idle"
        }
    }
}

/// How a task list orders its tasks: the Board by Recent, Created or Project; a project's
/// list and Chats by Recent, Created or Name.
enum BoardSort: String, CaseIterable, Identifiable {
    /// By last activity, a section a day.
    case recent
    /// By creation, a section a day.
    case created
    /// A section a project, each by last activity.
    case project
    /// By title, in one section.
    case name

    /// The Board's choices, and where it keeps its own.
    static let board: [BoardSort] = [.recent, .created, .project]
    static let boardKey = "boardSort"
    /// A project's list's and Chats' choices, and where they keep theirs.
    static let sessions: [BoardSort] = [.recent, .created, .name]
    static let sessionsKey = "sessionsSort"

    var id: Self { self }

    var title: String {
        switch self {
        case .recent: "Recent"
        case .created: "Created"
        case .project: "Project"
        case .name: "Name"
        }
    }
}

/// A run of a task list's tasks under one heading: a day, or a project; untitled when sorted
/// by name.
struct BoardSection: Hashable, Identifiable {
    var id: String { title }
    let title: String
    var trees: [TaskTree]

    /// `trees` in sections as `sort` orders them, the newest first, days dated in `calendar`.
    static func sections(_ trees: [TaskTree], by sort: BoardSort, now: Date,
                         calendar: Calendar = .current) -> [BoardSection] {
        switch sort {
        case .recent:
            return days(trees, at: \.lastActive, now: now, calendar: calendar)
        case .created:
            return days(trees, at: \.created, now: now, calendar: calendar)
        case .project:
            return Dictionary(grouping: trees, by: \.project)
                .map { BoardSection(title: $0.key.isEmpty ? "No project yet" : $0.key, trees: newest($0.value, at: \.lastActive)) }
                .sorted { $0.title.lowercased() < $1.title.lowercased() }
        case .name:
            guard !trees.isEmpty else { return [] }
            let named = newest(trees, at: \.lastActive).sorted {
                $0.lead.title.localizedStandardCompare($1.lead.title) == .orderedAscending
            }
            return [BoardSection(title: "", trees: named)]
        }
    }

    /// `trees` newest first by `date`, a section a day; those without a date last, as Earlier.
    private static func days(_ trees: [TaskTree], at date: (TaskTree) -> Date?, now: Date,
                             calendar: Calendar) -> [BoardSection] {
        var sections: [BoardSection] = []
        for tree in newest(trees, at: date) {
            let title = day(date(tree), now: now, calendar: calendar)
            if sections.last?.title == title {
                sections[sections.count - 1].trees.append(tree)
            } else {
                sections.append(BoardSection(title: title, trees: [tree]))
            }
        }
        return sections
    }

    /// `trees` newest first by `date`; ties, and trees without a date, newest created first.
    private static func newest(_ trees: [TaskTree], at date: (TaskTree) -> Date?) -> [TaskTree] {
        trees.sorted { a, b in
            let (x, y) = (date(a) ?? .distantPast, date(b) ?? .distantPast)
            if x != y { return x > y }
            // Session ids are ULIDs, which sort by creation time.
            return (a.lead.key.sessionId, a.lead.key.hostId) > (b.lead.key.sessionId, b.lead.key.hostId)
        }
    }

    /// A day's heading: Today, Yesterday, else the date, with its year when not this year's.
    static func day(_ date: Date?, now: Date, calendar: Calendar) -> String {
        guard let date else { return "Earlier" }
        if calendar.isDate(date, inSameDayAs: now) { return "Today" }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: now),
           calendar.isDate(date, inSameDayAs: yesterday) {
            return "Yesterday"
        }
        var style = Date.FormatStyle(timeZone: calendar.timeZone).weekday(.wide).month(.wide).day()
        style.calendar = calendar
        if !calendar.isDate(date, equalTo: now, toGranularity: .year) { style = style.year() }
        return date.formatted(style)
    }
}

/// The time a ULID was made, from its first ten characters: milliseconds since 1970 in
/// Crockford's base 32.
enum ULID {
    private static let digits = Array("0123456789ABCDEFGHJKMNPQRSTVWXYZ")

    static func date(_ id: String) -> Date? {
        guard id.count == 26 else { return nil }
        var milliseconds: UInt64 = 0
        for character in id.uppercased().prefix(10) {
            guard let digit = digits.firstIndex(of: character) else { return nil }
            milliseconds = milliseconds << 5 | UInt64(digit)
        }
        return Date(timeIntervalSince1970: Double(milliseconds) / 1000)
    }
}

extension Lists {
    /// Every task: each project's trees, with its name. A child is never a task of its own, though
    /// a project lists an archived child no live session hangs from as a tree of its own.
    private var tasks: [TaskTree] {
        let children = Set(self.children.values.joined())
        return projects.flatMap { project in
            (TaskTree.trees(project.live) + TaskTree.trees(project.archived))
                .filter { !children.contains($0.lead.key) }
                .map { tree in
                    var tree = tree
                    tree.project = project.projectId == nil ? "" : project.name
                    return tree
                }
        }
    }

    /// The Board's open tasks a search finds: those whose top session is neither archived nor
    /// moved.
    func board(matching query: String = "") -> [TaskTree] {
        tasks.filter { ![.archived, .moved].contains($0.lead.state) && $0.matches(query) }
    }

    /// The tasks archived today a search finds, as Show Archived lists them: archiving is a
    /// task's last activity.
    func archivedToday(matching query: String = "", now: Date = .now, calendar: Calendar = .current) -> [TaskTree] {
        tasks.filter { tree in
            tree.lead.state == .archived && tree.matches(query)
                && tree.lastActive.map { calendar.isDate($0, inSameDayAs: now) } == true
        }
        .sorted { ($0.lastActive ?? .distantPast) > ($1.lastActive ?? .distantPast) }
    }
}

/// The app's home: every open task, a task tree with its agents folded under its row, in
/// sections a day, newest first. Archiving a task completes it: the check on its row, E on the
/// Mac or a swipe on iOS, with Undo in the toast and on ⌘Z. A task opens beside the list, or is pushed.
struct BoardView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    /// Where an opened task opens on iPad and the Mac; `nil` pushes it.
    var selection: Binding<SessionKey?>?
    var query = ""
    @AppStorage(BoardSort.boardKey) private var sort = BoardSort.recent
    @State private var showsArchived = false
    /// The row J and K move to, and Return and E act on, on the Mac.
    @State private var cursor: SessionKey?
    @FocusState private var focused: Bool
    @Environment(\.undoManager) private var undoManager

    var body: some View {
        let lists = fleet.lists
        let sections = BoardSection.sections(lists.board(matching: query), by: sort, now: .now)
        let archived = lists.archivedToday(matching: query)
        let rows = sections.flatMap(\.trees) + (showsArchived ? archived : [])
        List {
            #if os(iOS)
            // On the phone; iPad's shell says it under the Board's title.
            if selection == nil { ConnectionLine(machines: lists.machines).taskListRow() }
            #endif
            if lists.machines.isEmpty {
                EmptyFleet { sheet = .pair }.taskListRow()
            } else if sections.isEmpty {
                Text(query.isEmpty ? "Nothing open." : "No tasks match.")
                    .foregroundStyle(Theme.tertiary)
                    .padding(.vertical, 24)
                    .taskListRow()
            }
            ForEach(sections) { section in
                Section {
                    ForEach(section.trees) { row($0, in: rows) }
                } header: {
                    SectionHeading(title: section.title, count: section.trees.count)
                }
            }
            if !archived.isEmpty {
                archivedLine(archived.count)
                if showsArchived {
                    ForEach(archived) { row($0, in: rows) }
                }
            }
            #if os(macOS)
            if !rows.isEmpty { keys }
            #endif
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .environment(\.defaultMinListRowHeight, 0)
        #if os(macOS)
        .frame(maxWidth: 760)
        .frame(maxWidth: .infinity)
        .focusable()
        .focusEffectDisabled()
        .focused($focused)
        .onAppear { focused = true }
        .onKeyPress(characters: .init(charactersIn: "jke"), phases: .down) { press in
            guard press.modifiers.isEmpty else { return .ignored }
            switch press.characters {
            case "j": move(1, in: rows)
            case "k": move(-1, in: rows)
            default: if let tree = rows.first(where: { $0.id == cursor }) { toggle(tree, in: rows) }
            }
            return .handled
        }
        .onKeyPress(.return) {
            guard let cursor else { return .ignored }
            selection?.wrappedValue = cursor
            return .handled
        }
        #endif
        .onChange(of: selection?.wrappedValue) { _, key in
            if let key { cursor = key }
        }
        .background(Theme.background)
        .refreshable { fleet.wake() }
        .navigationTitle("Board")
    }

    private func row(_ tree: TaskTree, in rows: [TaskTree]) -> some View {
        TaskListRow(tree: tree, fleet: fleet, selection: selection, context: tree.project, current: cursor == tree.id) {
            toggle(tree, in: rows)
        }
    }

    /// "Archived today", with the button that shows or hides those tasks.
    private func archivedLine(_ count: Int) -> some View {
        HStack(spacing: 8) {
            Text("Archived today · \(count)")
            Button(showsArchived ? "Hide Archived" : "Show Archived") {
                withAnimation(.snappy) { showsArchived.toggle() }
            }
            .buttonStyle(.plain)
            .underline()
            .foregroundStyle(Theme.secondary)
            Spacer()
        }
        .font(.caption)
        .foregroundStyle(Theme.tertiary)
        .padding(.vertical, 10)
        .padding(.horizontal, 8)
        .taskListRow()
    }

    #if os(macOS)
    private var keys: some View {
        HStack(spacing: 4) {
            KeyCap("J"); KeyCap("K"); Text("move ·")
            KeyCap("↩"); Text("open ·")
            KeyCap("E"); Text("archive ·")
            KeyCap("⌘Z"); Text("undo")
        }
        .font(.caption)
        .foregroundStyle(Theme.tertiary)
        .padding(.vertical, 14)
        .padding(.horizontal, 8)
        .taskListRow()
    }

    /// Moves the cursor `by` rows, from the first row when there is none.
    private func move(_ by: Int, in rows: [TaskTree]) {
        guard !rows.isEmpty else { return }
        guard let index = rows.firstIndex(where: { $0.id == cursor }) else {
            cursor = rows[0].id
            return
        }
        cursor = rows[min(max(index + by, 0), rows.count - 1)].id
    }
    #endif

    /// Archives an open task, or brings an archived one back; the cursor moves on to the next row.
    private func toggle(_ tree: TaskTree, in rows: [TaskTree]) {
        let key = tree.lead.key
        if tree.lead.state != .archived, cursor == key, let index = rows.firstIndex(where: { $0.id == key }) {
            let after = rows.dropFirst(index + 1).first { $0.lead.state != .archived } ?? rows.prefix(index).last
            cursor = after?.id
        }
        tree.toggleArchived(fleet: fleet, undoManager: undoManager)
    }
}

extension TaskTree {
    /// Archives an open task, its top session and so its tree, and brings an archived one back.
    /// An archive can be undone, from its toast or with ⌘Z.
    @MainActor func toggleArchived(fleet: Fleet, undoManager: UndoManager?) {
        let key = lead.key
        guard lead.state != .archived else {
            Task { await fleet.unarchive(key) }
            return
        }
        undoManager?.registerUndo(withTarget: fleet) { fleet in
            // The toast's Undo may have brought it back already.
            guard fleet.session(key)?.status == .archived else { return }
            Task { await fleet.unarchive(key) }
        }
        undoManager?.setActionName("Archive")
        Task { await fleet.archive(key) }
    }
}

/// A task list's sort menu, its choice kept under `key`: the Board's, or a project's list's
/// and Chats'.
struct BoardSortMenu: View {
    let choices: [BoardSort]
    @AppStorage private var sort: BoardSort

    init(_ choices: [BoardSort] = BoardSort.board, key: String = BoardSort.boardKey) {
        self.choices = choices
        _sort = AppStorage(wrappedValue: .recent, key)
    }

    var body: some View {
        Menu {
            Picker("Sort By", selection: $sort) {
                ForEach(choices) { Text($0.title).tag($0) }
            }
            .pickerStyle(.inline)
        } label: {
            Label(sort.title, systemImage: "arrow.up.arrow.down")
        }
        .accessibilityIdentifier("board-sort")
        .help("Sort the list")
    }
}

/// The tasks whose agents are unfolded under their row, as this device remembers them, one
/// "host/session" a line.
struct UnfoldedTasks {
    static let key = "unfoldedTasks"

    private(set) var stored: String

    init(_ stored: String) { self.stored = stored }

    private static func line(_ key: SessionKey) -> String { "\(key.hostId)/\(key.sessionId)" }

    func contains(_ key: SessionKey) -> Bool {
        stored.split(separator: "\n").contains { $0 == Self.line(key) }
    }

    /// Folds `key`'s agents when they are unfolded, else unfolds them.
    mutating func toggle(_ key: SessionKey) {
        var lines = stored.split(separator: "\n").map(String.init)
        if let index = lines.firstIndex(of: Self.line(key)) { lines.remove(at: index) } else { lines.append(Self.line(key)) }
        stored = lines.joined(separator: "\n")
    }
}

extension TaskTree {
    /// What its row's second line says: `context`, then what the task is doing.
    func line(context: String) -> String {
        [context, activity].filter { !$0.isEmpty }.joined(separator: " · ")
    }

    /// "1 agent", "3 agents".
    var agentsLabel: String { agentCount == 1 ? "1 agent" : "\(agentCount) agents" }
}

extension TaskState {
    /// Where a provider's agent stands: working while it runs, else idle.
    init(agent outcome: ToolCall.Outcome) {
        self = outcome == .running ? .working : .idle
    }
}

/// A task's rows in a task list, as the Board, Chats and a project's list draw them: the task,
/// highlighted while it is the Mac's keyboard row or open, then, when unfolded, each of its
/// agents, a row of its own that opens it. Which tasks are unfolded is kept on this device. A
/// task is archived from the check its row shows on hover, a swipe on iOS or its context menu,
/// where it is renamed too.
struct TaskListRow: View {
    let tree: TaskTree
    let fleet: Fleet
    let selection: Binding<SessionKey?>?
    /// What its line says before what the task is doing: the Board's project, a project's
    /// branch.
    let context: String
    /// Whether it is the Mac's keyboard row.
    var current = false
    let toggle: () -> Void
    @AppStorage(UnfoldedTasks.key) private var unfolded = ""
    @Environment(\.pages) private var pages
    @Environment(\.sessionPath) private var path

    var body: some View {
        let archived = tree.lead.state == .archived
        let open = tree.agentCount > 0 && UnfoldedTasks(unfolded).contains(tree.id)
        TaskRow(tree: tree, fleet: fleet, selection: selection, context: context, current: current,
                unfolded: open, unfold: unfold, toggle: toggle)
            .listRowInsets(EdgeInsets(top: 0, leading: 8, bottom: 0, trailing: 8))
            .listRowSeparator(.hidden)
            .listRowBackground(highlight(current || selection?.wrappedValue == tree.id))
            #if os(iOS)
            .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                Button(archived ? "Unarchive" : "Archive", systemImage: archived ? "tray.and.arrow.up" : "archivebox",
                       action: toggle)
                    .tint(archived ? Theme.waiting : Theme.success)
            }
            #endif
            .contextMenu {
                if tree.lead.state.renamable {
                    Button("Rename…", systemImage: "pencil") { fleet.renaming = tree.lead.key }
                }
                Button(archived ? "Unarchive" : "Archive", systemImage: archived ? "tray.and.arrow.up" : "archivebox",
                       action: toggle)
            }
        if open {
            let provider = fleet.session(tree.lead.key)?.provider ?? "claude"
            ForEach(tree.lead.agents) { agent in
                TaskAgentRow(title: agent.title, provider: provider, state: TaskState(agent: agent.outcome), depth: 1,
                             label: "Open sub-agent: \(agent.title), \(agent.status)") {
                    pages.open(.agent(tree.lead.key, agent.id))
                }
                .help(agent.prompt)
                .agentListRow(highlight(false))
            }
            ForEach(tree.listed) { child in
                SessionButton(key: child.key, open: sessionOpener(selection: selection, path: path)) {
                    TaskAgentRow(title: child.title, provider: fleet.session(child.key)?.provider ?? "claude",
                                 state: TaskState(child.state), depth: max(child.depth, 1),
                                 label: "Open sub-session: \(child.title), \(child.state.label)")
                        .opacity(child.state == .archived ? 0.6 : 1)
                }
                .agentListRow(highlight(selection?.wrappedValue == child.key))
            }
        }
    }

    private func unfold() {
        var tasks = UnfoldedTasks(unfolded)
        tasks.toggle(tree.id)
        withAnimation(.snappy) { unfolded = tasks.stored }
    }

    private func highlight(_ on: Bool) -> some View {
        RoundedRectangle(cornerRadius: Theme.corner - 2)
            .fill(on ? Theme.raised : .clear)
            .padding(.horizontal, 4)
    }
}

/// A task: where it stands, its title over the full width, two lines at most, and how long
/// since it last did something; under them its context, what it is doing, and how many agents
/// work under it, which unfold under it. On hover the age gives way to the check that archives
/// it. It opens its top session, into `selection` when given, else by pushing it.
private struct TaskRow: View {
    let tree: TaskTree
    let fleet: Fleet
    let selection: Binding<SessionKey?>?
    let context: String
    /// Whether it is the Mac's keyboard row.
    let current: Bool
    let unfolded: Bool
    let unfold: () -> Void
    let toggle: () -> Void
    @State private var hovering = false
    @Environment(\.sessionPath) private var path

    var body: some View {
        let state = tree.state
        let line = tree.line(context: context)
        HStack(alignment: .top, spacing: 10) {
            TaskGlyph(state: state).padding(.top, 1)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(tree.lead.title)
                        .font(Theme.title)
                        .foregroundStyle(Theme.text)
                        .lineLimit(2)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    trailing
                }
                HStack(spacing: 6) {
                    if !line.isEmpty {
                        Text(line)
                            .font(Theme.body)
                            .foregroundStyle(state == .waitingForYou ? Theme.secondary : Theme.tertiary)
                            .lineLimit(1)
                    }
                    if tree.agentCount > 0 { agents(after: !line.isEmpty) }
                }
            }
        }
        .padding(.vertical, 8)
        .opacity(tree.lead.state == .archived ? 0.6 : 1)
        .background { opener(label: "\(state.label), \(tree.lead.title), \(line)"
            + (tree.lastActive.map { ", \(RelativeTime.text($0))" } ?? "")) }
        .onHover { hovering = $0 }
    }

    /// The age, or on hover the check that archives the task, or brings it back once archived.
    private var trailing: some View {
        let archived = tree.lead.state == .archived
        return ZStack(alignment: .trailing) {
            if let lastActive = tree.lastActive {
                RelativeTime(date: lastActive)
                    .font(Theme.meta)
                    .foregroundStyle(Theme.tertiary)
                    .fixedSize()
                    .opacity(hovering ? 0 : 1)
                    .accessibilityHidden(true)
            }
            if hovering {
                Button(action: toggle) {
                    ArchiveCircle(archived: archived)
                        .contentShape(.rect.inset(by: -6))
                }
                .buttonStyle(.plain)
                .help(archived ? "Unarchive" : "Archive")
                .accessibilityLabel(archived ? "Unarchive \(tree.lead.title)" : "Archive \(tree.lead.title)")
            }
        }
    }

    /// "· ▸ 3 agents", which unfolds the task's agents under its row.
    private func agents(after line: Bool) -> some View {
        Button(action: unfold) {
            HStack(spacing: 4) {
                if line { Text("·") }
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .bold))
                    .rotationEffect(.degrees(unfolded ? 90 : 0))
                Text(tree.agentsLabel)
            }
            .font(Theme.body)
            .foregroundStyle(Theme.secondary)
            .contentShape(.rect.inset(by: -6))
        }
        .buttonStyle(.plain)
        .fixedSize()
        .layoutPriority(1)
        .accessibilityIdentifier("Agents of \(tree.lead.title)")
        .accessibilityLabel("\(unfolded ? "Hide" : "Show") \(tree.agentsLabel)")
    }

    /// The row's open action, behind its title and lines, so the agents and the check stay
    /// buttons of their own.
    private func opener(label: String) -> some View {
        SessionButton(key: tree.lead.key, open: sessionOpener(selection: selection, path: path)) {
            Color.clear.contentShape(.rect)
        }
        .accessibilityLabel(label)
    }
}

/// How a task row opens a session: into `selection` on the Mac and iPad, else pushed onto the
/// tab's `path` on the iPhone, where a NavigationLink in the row never fires; `nil` when neither
/// is given, for a NavigationLink.
private func sessionOpener(selection: Binding<SessionKey?>?, path: Binding<[NavRoute]>?) -> ((SessionKey) -> Void)? {
    if let selection { return { selection.wrappedValue = $0 } }
    if let path { return { path.wrappedValue.append(.session($0)) } }
    return nil
}

/// One of a task's agents under its row: the line that ties it to the task, its provider's
/// mark, its title and where it stands.
private struct TaskAgentRow: View {
    let title: String
    let provider: Provider
    let state: TaskState
    let depth: Int
    let label: String
    var open: (() -> Void)?
    @State private var hovering = false

    var body: some View {
        if let open {
            Button(action: open) { row }.buttonStyle(.plain)
        } else {
            row
        }
    }

    private var row: some View {
        HStack(spacing: 8) {
            TreeLine().frame(width: CGFloat(depth) * 14)
            ProviderMark(provider: provider, size: 11)
                .frame(width: 20, height: 20)
                .background(Theme.raised, in: Circle())
                .overlay(Circle().strokeBorder(Theme.stroke))
            Text(title).font(Theme.body).foregroundStyle(Theme.text).lineLimit(1)
            Spacer(minLength: 8)
            TaskGlyph(state: state)
        }
        .padding(.vertical, 4)
        .background(hovering ? Theme.raised.opacity(0.6) : .clear, in: .rect(cornerRadius: 7))
        .contentShape(.rect)
        .onHover { hovering = $0 }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(label)
    }
}

/// Where a task stands as one small mark: working ◐, waiting for you a small amber dot, idle ○.
struct TaskGlyph: View {
    let state: TaskState

    var body: some View {
        Group {
            switch state {
            case .working:
                Image(systemName: "circle.lefthalf.filled").font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.running)
            case .waitingForYou:
                Circle().fill(Theme.accent).frame(width: 7, height: 7)
            case .idle:
                Image(systemName: "circle").font(.system(size: 12)).foregroundStyle(Theme.idle)
            }
        }
        .frame(width: 16, height: 16)
        .accessibilityLabel(state.label)
    }
}

/// The check that archives a task, filled once archived.
private struct ArchiveCircle: View {
    let archived: Bool

    var body: some View {
        ZStack {
            Circle().fill(archived ? Theme.secondary : .clear)
            Circle().strokeBorder(Theme.secondary, lineWidth: 1.5)
            Image(systemName: "checkmark").font(.system(size: 8, weight: .bold))
                .foregroundStyle(archived ? Theme.background : Theme.secondary)
        }
        .frame(width: 16, height: 16)
    }
}

#if os(macOS)
/// A key as the Board's legend shows it.
private struct KeyCap: View {
    let key: String

    init(_ key: String) { self.key = key }

    var body: some View {
        Text(key)
            .font(.caption2.monospaced())
            .padding(.horizontal, 4)
            .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(Theme.stroke))
    }
}
#endif

extension View {
    /// A row of a task's agent under the task: no separator, the task's insets.
    fileprivate func agentListRow(_ background: some View) -> some View {
        listRowInsets(EdgeInsets(top: 0, leading: 8, bottom: 0, trailing: 8))
            .listRowSeparator(.hidden)
            .listRowBackground(background)
    }

    /// A row of a task list that is not a task: no inset, background or separator.
    func taskListRow() -> some View {
        listRowInsets(EdgeInsets(top: 0, leading: 8, bottom: 0, trailing: 8))
            .listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
    }
}
