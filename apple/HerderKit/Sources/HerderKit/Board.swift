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

/// How the Board orders its tasks.
enum BoardSort: String, CaseIterable, Identifiable {
    /// By last activity, a section a day.
    case recent
    /// By creation, a section a day.
    case created
    /// A section a project, each by last activity.
    case project

    var id: Self { self }

    var title: String {
        switch self {
        case .recent: "Recent"
        case .created: "Created"
        case .project: "Project"
        }
    }
}

/// A run of the Board's tasks under one heading: a day, or a project.
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

/// The app's home: every open task, a task tree with its agents folded into its row, in
/// sections a day, newest first. Archiving a task completes it: its circle, E on the Mac or a
/// swipe on iOS, with Undo in the toast and on ⌘Z. A task opens beside the list, or is pushed.
struct BoardView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    /// Where an opened task opens on iPad and the Mac; `nil` pushes it.
    var selection: Binding<SessionKey?>?
    var query = ""
    @AppStorage("boardSort") private var sort = BoardSort.recent
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
            if selection == nil { ConnectionLine(machines: lists.machines).boardRow() }
            #endif
            if lists.machines.isEmpty {
                EmptyFleet { sheet = .pair }.boardRow()
            } else if sections.isEmpty {
                Text(query.isEmpty ? "Nothing open." : "No tasks match.")
                    .foregroundStyle(Theme.tertiary)
                    .padding(.vertical, 24)
                    .boardRow()
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
        let archived = tree.lead.state == .archived
        let current = cursor == tree.id || selection?.wrappedValue == tree.id
        return TaskRow(tree: tree, fleet: fleet, selection: selection, current: cursor == tree.id) {
            toggle(tree, in: rows)
        }
        .listRowInsets(EdgeInsets(top: 0, leading: 8, bottom: 0, trailing: 8))
        .listRowSeparator(.hidden)
        .listRowBackground(
            RoundedRectangle(cornerRadius: Theme.corner - 2)
                .fill(current ? Theme.raised : .clear)
                .padding(.horizontal, 4))
        #if os(iOS)
        .swipeActions(edge: .trailing, allowsFullSwipe: true) {
            Button(archived ? "Unarchive" : "Archive", systemImage: archived ? "tray.and.arrow.up" : "archivebox") {
                toggle(tree, in: rows)
            }
            .tint(archived ? Theme.waiting : Theme.success)
        }
        #endif
        .contextMenu {
            if tree.lead.state.renamable {
                Button("Rename…", systemImage: "pencil") { fleet.renaming = tree.lead.key }
            }
            Button(archived ? "Unarchive" : "Archive", systemImage: archived ? "tray.and.arrow.up" : "archivebox") {
                toggle(tree, in: rows)
            }
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
        .boardRow()
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
        .boardRow()
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

    /// Archives an open task, its top session and so its tree, and brings an archived one back.
    /// An archive can be undone, from its toast or with ⌘Z; the cursor moves on to the next row.
    private func toggle(_ tree: TaskTree, in rows: [TaskTree]) {
        let key = tree.lead.key
        guard tree.lead.state != .archived else {
            Task { await fleet.unarchive(key) }
            return
        }
        if cursor == key, let index = rows.firstIndex(where: { $0.id == key }) {
            let after = rows.dropFirst(index + 1).first { $0.lead.state != .archived } ?? rows.prefix(index).last
            cursor = after?.id
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

/// The Board's sort menu: Recent, Created or Project.
struct BoardSortMenu: View {
    @AppStorage("boardSort") private var sort = BoardSort.recent

    var body: some View {
        Menu {
            Picker("Sort By", selection: $sort) {
                ForEach(BoardSort.allCases) { Text($0.title).tag($0) }
            }
            .pickerStyle(.inline)
        } label: {
            Label(sort.title, systemImage: "arrow.up.arrow.down")
        }
        .accessibilityIdentifier("board-sort")
        .help("Sort the Board")
    }
}

/// A task on the Board: the circle that archives it, where it stands, its title, its project
/// and what it is doing, how many agents work under it, and how long since it last did
/// something. It opens its top session, into `selection` when given, else by pushing it.
private struct TaskRow: View {
    let tree: TaskTree
    let fleet: Fleet
    let selection: Binding<SessionKey?>?
    /// Whether it is the Mac's keyboard row.
    let current: Bool
    let toggle: () -> Void
    @State private var hovering = false

    var body: some View {
        let archived = tree.lead.state == .archived
        HStack(spacing: 10) {
            Button(action: toggle) {
                ArchiveCircle(archived: archived, hovering: hovering).hitTarget()
            }
            .buttonStyle(.plain)
            .help(archived ? "Unarchive" : "Archive")
            .accessibilityLabel(archived ? "Unarchive \(tree.lead.title)" : "Archive \(tree.lead.title)")
            if let selection {
                Button { selection.wrappedValue = tree.lead.key } label: { content }
                    .buttonStyle(.plain)
            } else {
                NavigationLink(value: NavRoute.session(tree.lead.key)) { content }
                    .buttonStyle(.plain)
            }
        }
        .opacity(archived ? 0.6 : 1)
        .onHover { hovering = $0 }
    }

    private var content: some View {
        let state = tree.state
        let line = tree.project.isEmpty ? tree.activity : "\(tree.project) · \(tree.activity)"
        return HStack(spacing: 10) {
            TaskGlyph(state: state)
            VStack(alignment: .leading, spacing: 2) {
                Text(tree.lead.title)
                    .font(Theme.title)
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Text(line)
                    .font(Theme.body)
                    .foregroundStyle(state == .waitingForYou ? Theme.secondary : Theme.tertiary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            Group {
                #if os(macOS)
                if current { Text("E archive") }
                #endif
                if tree.agentCount > 0 {
                    Text(tree.agentCount == 1 ? "1 agent" : "\(tree.agentCount) agents")
                }
                if let lastActive = tree.lastActive { RelativeTime(date: lastActive) }
            }
            .font(Theme.meta)
            .foregroundStyle(Theme.tertiary)
        }
        .padding(.vertical, 8)
        .contentShape(.rect)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(state.label), \(tree.lead.title), \(line)"
            + (tree.lastActive.map { ", \(RelativeTime.text($0))" } ?? ""))
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

/// The circle that archives a task: a check shows in it on hover, and fills it once archived.
private struct ArchiveCircle: View {
    let archived: Bool
    let hovering: Bool

    var body: some View {
        ZStack {
            Circle().fill(archived ? Theme.idle : .clear)
            Circle().strokeBorder(hovering || archived ? Theme.secondary : Theme.idle, lineWidth: 1.5)
            if hovering || archived {
                Image(systemName: "checkmark").font(.system(size: 8, weight: .bold))
                    .foregroundStyle(archived ? Theme.background : Theme.secondary)
            }
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

private extension View {
    /// A row of the Board's list that is not a task: no inset, background or separator.
    func boardRow() -> some View {
        listRowInsets(EdgeInsets(top: 0, leading: 8, bottom: 0, trailing: 8))
            .listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
    }
}

/// Task trees as one card, each tree's agents folded under its top. An agent opened from
/// elsewhere unfolds its tree.
struct TaskTreeList: View {
    let trees: [TaskTree]
    let fleet: Fleet
    /// Where a tapped session opens beside the list; `nil` pushes it.
    let selection: Binding<SessionKey?>?
    var showsProject = true
    /// The trees whose agents are unfolded; every tree starts folded.
    @State private var unfolded: Set<SessionKey> = []

    var body: some View {
        if !trees.isEmpty {
            VStack(spacing: 0) {
                ForEach(Array(trees.enumerated()), id: \.element.id) { index, tree in
                    if index > 0 { Divider().overlay(Theme.stroke).padding(.leading, 42) }
                    TaskTreeRows(tree: tree, fleet: fleet, selection: selection, showsProject: showsProject,
                                 unfolded: unfolding(tree))
                }
            }
            .padding(4)
            .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
            .onChange(of: selection?.wrappedValue, initial: true) { _, key in
                guard let key, let tree = trees.first(where: { $0.children.contains { $0.key == key } }) else { return }
                unfolded.insert(tree.id)
            }
        }
    }

    private func unfolding(_ tree: TaskTree) -> Binding<Bool> {
        Binding {
            unfolded.contains(tree.id)
        } set: { open in
            if open { unfolded.insert(tree.id) } else { unfolded.remove(tree.id) }
        }
    }
}

/// A task tree's rows: its top session's row, then one row that folds its agents, its
/// provider's own and its herder children, the live ones before the archived.
private struct TaskTreeRows: View {
    let tree: TaskTree
    let fleet: Fleet
    let selection: Binding<SessionKey?>?
    let showsProject: Bool
    @Binding var unfolded: Bool

    var body: some View {
        SessionLink(session: tree.lead, fleet: fleet, selection: selection, showsProject: showsProject)
        if tree.agentCount > 0 {
            fold
            if unfolded {
                ForEach(tree.lead.agents) { agent in
                    NativeAgentRow(agent: agent, fleet: fleet, key: tree.lead.key)
                }
                ForEach(tree.listed) { child in
                    TaskChildRow(session: child, fleet: fleet, selection: selection)
                }
            }
        }
    }

    private var fold: some View {
        let count = tree.agentCount
        let state = tree.agentsState
        return Button { unfolded.toggle() } label: {
            HStack(spacing: 8) {
                TreeLine().frame(width: 14)
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.bold))
                    .rotationEffect(.degrees(unfolded ? 90 : 0))
                    .frame(width: 20, height: 20)
                Text(count == 1 ? "1 agent" : "\(count) agents").font(.subheadline)
                Spacer(minLength: 6)
                if let state {
                    if state == .running { ProgressView().controlSize(.mini) }
                    Text(state.label).font(.caption.weight(.semibold)).foregroundStyle(state.tint).fixedSize()
                }
            }
            .foregroundStyle(Theme.secondary)
            .padding(.vertical, 5)
            .padding(.horizontal, 8)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("Agents of \(tree.lead.title)")
        .accessibilityLabel("\(unfolded ? "Hide" : "Show") \(count == 1 ? "1 agent" : "\(count) agents")"
            + (state.map { ", \($0.label)" } ?? ""))
    }
}

/// A herder child under the top of its task tree, drawn as a provider's agent is
/// (`NativeAgentRow`), badged with its state. It opens the child, into `selection` when given,
/// else by pushing it, as `SessionLink` does.
private struct TaskChildRow: View {
    let session: SessionSummary
    let fleet: Fleet
    let selection: Binding<SessionKey?>?
    @State private var hovering = false

    var body: some View {
        if let selection {
            Button { selection.wrappedValue = session.key } label: { row(selected: selection.wrappedValue == session.key) }
                .buttonStyle(.plain)
        } else {
            NavigationLink(value: NavRoute.session(session.key)) { row(selected: false) }
                .buttonStyle(.plain)
        }
    }

    @ViewBuilder private func row(selected: Bool) -> some View {
        let archived = session.state == .archived
        HStack(spacing: 8) {
            TreeLine().frame(width: CGFloat(max(session.depth, 1)) * 14)
            ProviderMark(provider: fleet.session(session.key)?.provider ?? "claude", size: 11)
                .frame(width: 20, height: 20)
                .background(Theme.raised, in: Circle())
                .overlay(Circle().strokeBorder(Theme.stroke))
            Text(session.title).font(.subheadline).foregroundStyle(Theme.text).lineLimit(1)
            Spacer(minLength: 6)
            if session.state == .running { ProgressView().controlSize(.mini) }
            Text(session.state.label).font(.caption.weight(.semibold))
                .foregroundStyle(archived ? Theme.tertiary : session.state.tint)
                .fixedSize()
        }
        .padding(.vertical, 5)
        .padding(.horizontal, 8)
        .background(selected || hovering ? Theme.raised : .clear, in: .rect(cornerRadius: 7))
        .contentShape(.rect)
        .opacity(archived ? 0.75 : 1)
        .onHover { hovering = $0 }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Open sub-session: \(session.title), \(session.state.label)")
    }
}
