import Herder
import SwiftUI

/// The sidebar's entries.
enum SidebarItem: Hashable {
    case home, board, pullRequests, usage, skills, machines, vault
    case project(String)
}

/// The tab bar's tabs on compact width: the sidebar's sections, with the projects as one tab,
/// Pull Requests inside the Board, and Skills and a vault inside Machines.
enum CompactTab: Hashable, CaseIterable {
    case home, projects, board, usage, machines

    /// The tab that shows a sidebar entry.
    init(_ item: SidebarItem) {
        switch item {
        case .home: self = .home
        case .project: self = .projects
        case .board, .pullRequests: self = .board
        case .usage: self = .usage
        case .skills, .machines, .vault: self = .machines
        }
    }

    var title: String {
        switch self {
        case .home: "Home"
        case .projects: "Projects"
        case .board: "Board"
        case .usage: "Usage"
        case .machines: "Machines"
        }
    }

    var symbol: String {
        switch self {
        case .home: "tray.full"
        case .projects: "square.stack.3d.up"
        case .board: "checklist"
        case .usage: "chart.bar"
        case .machines: "server.rack"
        }
    }
}

/// The fleet: tabs on iPhone; herder's own sidebar and panes on iPad and the Mac.
struct FleetView: View {
    let fleet: Fleet
    @State private var sheet: AppSheet?
    @State private var item: SidebarItem = .home
    @State private var session: SessionKey?
    @State private var draft: Draft?
    @State private var tab = CompactTab.home
    @State private var homePath: [SessionKey] = []
    @State private var projectsPath: [SessionKey] = []
    @State private var boardPath: [SessionKey] = []
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    var body: some View {
        Group {
            #if os(iOS)
            if sizeClass == .compact { tabs } else { shell }
            #else
            shell
            #endif
        }
        .tint(Theme.text)
        #if os(iOS)
        // An iPad going compact keeps the section it showed.
        .onChange(of: sizeClass) { if sizeClass == .compact { tab = CompactTab(item) } }
        #endif
        .sheet(item: $sheet) { sheet in
            sheet.view(fleet: fleet) { draft = $0 }
        }
        #if os(iOS)
        .fullScreenCover(item: Binding(get: { sizeClass == .compact ? draft : nil }, set: { draft = $0 })) { draft in
            NavigationStack {
                DraftSessionView(fleet: fleet, draft: draft) { opened($0) }
                    .toolbar { Button("Cancel") { self.draft = nil } }
            }
        }
        #endif
        .task { await fleet.follow() }
        .overlay(alignment: .bottom) { ToastView(fleet: fleet) }
        .alert(fleet.archiveRefusal.map { "Couldn’t archive “\($0.title)”" } ?? "",
               isPresented: Binding(get: { fleet.archiveRefusal != nil }, set: { if !$0 { fleet.archiveRefusal = nil } }),
               presenting: fleet.archiveRefusal) { _ in
            Button("OK", role: .cancel) {}
        } message: { refusal in
            Text(refusal.reason)
        }
        .onChange(of: draft) {
            // A draft shows in a list's session pane; Machines has none.
            if let draft {
                session = nil
                // The list beside the chat is the draft's project, or Home for a path.
                let shown = draft.projectId.map { id in fleet.lists.projects.contains { $0.id == id } } ?? false
                item = shown ? .project(draft.projectId ?? "") : .home
            }
        }
        .onChange(of: session) { if session != nil { draft = nil } }
        // A removed project's pane has nothing left to show.
        .onChange(of: fleet.lists.projects.map(\.id)) { _, ids in
            if case .project(let id) = item, !ids.contains(id) { item = .home }
        }
        // A draft belongs to Home or its own project; leaving for elsewhere drops it.
        .onChange(of: item) {
            guard let draft else { return }
            if item != .home && item != .project(draft.projectId ?? "") { self.draft = nil }
        }
    }

    private var shell: some View {
        DesktopShell(fleet: fleet, sheet: $sheet, item: $item, session: $session, draft: $draft, opened: opened)
    }

    /// Shows a session just created from a draft: in its project's pane, or pushed on Home.
    private func opened(_ key: SessionKey) {
        draft = nil
        if let projectId = fleet.lists.projects.first(where: { $0.sessions.contains { $0.key == key } })?.projectId {
            item = .project(projectId)
        }
        session = key
        tab = .home
        homePath = [key]
    }

    #if os(iOS)
    private var tabs: some View {
        TabView(selection: $tab) {
            ForEach(CompactTab.allCases, id: \.self) { tab in
                self.tab(tab)
                    .tabItem { Label(tab.title, systemImage: tab.symbol) }
                    .badge(tab == .home ? fleet.lists.requests.count : 0)
                    .tag(tab)
            }
        }
    }

    @ViewBuilder private func tab(_ tab: CompactTab) -> some View {
        switch tab {
        case .home:
            NavigationStack(path: $homePath) {
                HomeView(fleet: fleet, sheet: $sheet)
                    .toolbar { Button("New Session", systemImage: "plus") { sheet = .newSession } }
            }
            .environment(\.sessionPath, $homePath)
        case .projects:
            NavigationStack(path: $projectsPath) {
                ProjectsView(fleet: fleet, sheet: $sheet, draft: $draft, projects: fleet.lists.projects)
                    .toolbar { Button("New Project", systemImage: "plus") { sheet = .newProject } }
            }
            .environment(\.sessionPath, $projectsPath)
        case .board:
            NavigationStack(path: $boardPath) { BoardView(fleet: fleet, showsPullRequests: true) }
                .environment(\.sessionPath, $boardPath)
        case .usage:
            NavigationStack { UsageView(fleet: fleet) }
        case .machines:
            NavigationStack { MachinesView(fleet: fleet, sheet: $sheet, showsVaults: true, showsSkills: true) }
        }
    }
    #endif
}

/// What needs you, then every session that is not archived, in the order they were created.
struct HomeView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    /// Where a tapped session opens on iPad and the Mac; `nil` pushes it.
    var selection: Binding<SessionKey?>?
    /// Keeps the sessions that match.
    var query = ""

    var body: some View {
        let lists = fleet.lists
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 22) {
                #if os(iOS)
                ConnectionLine(machines: lists.machines)
                #endif
                if lists.machines.isEmpty {
                    EmptyFleet { sheet = .pair }
                }
                if !lists.requests.isEmpty && query.isEmpty {
                    VStack(alignment: .leading, spacing: 10) {
                        SectionHeading(title: "Needs you", count: lists.requests.count, tint: Theme.accent)
                        ForEach(lists.requests) { RequestCard(request: $0, fleet: fleet, selection: selection) }
                    }
                }
                SessionGroup(title: "Sessions", sessions: lists.home.filter { $0.matches(query) }, fleet: fleet,
                             selection: selection)
            }
            .frame(maxWidth: 760)
            .frame(maxWidth: .infinity)
            .padding(.horizontal, 16)
            .padding(.bottom, 24)
        }
        .background(Theme.background)
        .refreshable { fleet.wake() }
        .navigationTitle("herder")
        #if os(macOS)
        .navigationSubtitle(ConnectionLine.text(lists.machines))
        #endif
        .navigationDestination(for: SessionKey.self) { SessionView(fleet: fleet, key: $0) }
    }
}

/// "2 of 3 machines connected".
private struct ConnectionLine: View {
    let machines: [MachineSummary]

    static func text(_ machines: [MachineSummary]) -> String {
        let connected = machines.filter(\.connected).count
        if machines.isEmpty { return "" }
        if connected < machines.count { return "\(connected) of \(machines.count) machines connected" }
        return machines.count == 1 ? "1 machine connected" : "All \(machines.count) machines connected"
    }

    var body: some View {
        let connected = machines.filter(\.connected).count
        if !machines.isEmpty {
            HStack(spacing: 8) {
                Circle().fill(connected == machines.count ? Theme.success : Theme.accent)
                    .frame(width: 7, height: 7)
                Text(Self.text(machines))
            }
            .font(.footnote.weight(.medium))
            .foregroundStyle(Theme.secondary)
        }
    }
}

private struct EmptyFleet: View {
    let add: () -> Void

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 12) {
                Text("No machines yet").font(.headline).foregroundStyle(Theme.text)
                Text("Run `herder pair` on a machine, then add it here with the link it prints.")
                    .font(.subheadline)
                    .foregroundStyle(Theme.secondary)
                ActionButton(title: "Add Machine", style: .primary) { add() }
                    #if os(macOS)
                    .frame(maxWidth: 220)
                    #endif
            }
        }
    }
}

/// A card of session rows, under its title when it has one.
struct SessionGroup: View {
    let title: String?
    let sessions: [SessionSummary]
    let fleet: Fleet
    var selection: Binding<SessionKey?>?
    var showsProject = true

    var body: some View {
        if !sessions.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                if let title { SectionHeading(title: title, count: sessions.count) }
                VStack(spacing: 0) {
                    ForEach(Array(sessions.enumerated()), id: \.element.id) { index, session in
                        if index > 0 { Divider().overlay(Theme.stroke).padding(.leading, 42) }
                        SessionLink(session: session, fleet: fleet, selection: selection, showsProject: showsProject)
                        ForEach(session.agents) { agent in
                            NativeAgentRow(agent: agent, fleet: fleet, key: session.key, depth: session.depth)
                        }
                    }
                }
                .padding(4)
                .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
            }
        }
    }
}

/// A session row that opens the session, into `selection` when given, else by pushing it,
/// with archiving at hand: a button on hover, and in the context menu.
struct SessionLink: View {
    let session: SessionSummary
    let fleet: Fleet
    let selection: Binding<SessionKey?>?
    var showsProject = true
    @State private var hovering = false

    var body: some View {
        Group {
            if let selection {
                Button { selection.wrappedValue = session.key } label: { row }
                    .buttonStyle(.plain)
                    .background(
                        selection.wrappedValue == session.key || hovering ? Theme.raised : .clear,
                        in: .rect(cornerRadius: Theme.corner - 2))
            } else {
                NavigationLink(value: session.key) { row }
                    .buttonStyle(.plain)
            }
        }
        .opacity(fleet.archiving.contains(session.key) ? 0.5 : 1)
        .overlay(alignment: .topTrailing) {
            if fleet.archiving.contains(session.key) {
                ArchivingLabel().padding(8)
            } else if hovering && session.state != .archived {
                IconButton(symbol: "archivebox", help: "Archive") { Task { await fleet.archive(session.key) } }
                    .padding(8)
            }
        }
        .onHover { hovering = $0 }
        .contextMenu {
            if session.state != .archived {
                Button("Archive", systemImage: "archivebox") { Task { await fleet.archive(session.key) } }
            }
        }
    }

    private var row: some View {
        SessionRow(session: session, showsProject: showsProject)
            .padding(.horizontal, 8)
    }
}

/// The projects as compact rows, most recently active first, each unfolding to its sessions
/// in task-tree order: the live ones, then the archived ones folded away at the end. A search
/// keeps the projects whose name or path matches, and those with sessions that match,
/// unfolded to show just those sessions.
struct ProjectsView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    @Binding var draft: Draft?
    let projects: [ProjectGroup]
    var title = "Projects"
    @State private var query = ""
    /// What the user unfolded: project ids, and `archived:` ids for their archived sessions.
    @State private var unfolded: Set<String> = []
    /// The same while a search runs, starting with what it found through sessions unfolded.
    @State private var searchUnfolded: Set<String> = []

    private var searching: Bool { !query.trimmingCharacters(in: .whitespaces).isEmpty }

    var body: some View {
        let shown = ProjectGroup.found(projects, query: query)
        List {
            ForEach(shown) { project in
                DisclosureGroup(isExpanded: folding(project.id)) {
                    ForEach(project.live) { session in
                        row(session)
                        ForEach(session.agents) { agent in
                            NativeAgentRow(agent: agent, fleet: fleet, key: session.key, depth: session.depth)
                                .listRowBackground(Theme.surface)
                        }
                    }
                    if !project.archived.isEmpty {
                        DisclosureGroup(isExpanded: folding("archived:" + project.id)) {
                            ForEach(project.archived) { row($0) }
                        } label: {
                            Text("Archived · \(project.archived.count)")
                                .font(.subheadline.weight(.semibold)).foregroundStyle(Theme.secondary)
                        }
                        .listRowBackground(Theme.surface)
                    }
                } label: {
                    header(project)
                }
                .listRowBackground(Theme.surface)
            }
        }
        .environment(\.defaultMinListRowHeight, 36)
        .overlay {
            if projects.isEmpty {
                ContentUnavailableView("No sessions", systemImage: "square.stack.3d.up",
                                       description: Text("Sessions on your machines appear here."))
            } else if shown.isEmpty {
                ContentUnavailableView.search(text: query)
            }
        }
        .searchable(text: $query, prompt: "Projects and sessions")
        .onChange(of: query) {
            // What a search finds only through sessions opens to show them.
            let found = ProjectGroup.found(projects, query: query).filter { !$0.matches(query) }
            searchUnfolded = Set(found.map(\.id) + found.filter { !$0.archived.isEmpty }.map { "archived:" + $0.id })
        }
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .refreshable { fleet.wake() }
        .navigationTitle(title)
        .navigationDestination(for: SessionKey.self) { SessionView(fleet: fleet, key: $0) }
    }

    /// Whether `id` is unfolded: the user's own choice, or while searching the search's.
    private func folding(_ id: String) -> Binding<Bool> {
        Binding(
            get: { searching ? searchUnfolded.contains(id) : unfolded.contains(id) },
            set: { open in
                if searching {
                    if open { searchUnfolded.insert(id) } else { searchUnfolded.remove(id) }
                } else {
                    if open { unfolded.insert(id) } else { unfolded.remove(id) }
                }
            })
    }

    /// A project's one compact row: its icon and name, its live sessions and machines, and how
    /// long since anything happened in it.
    private func header(_ project: ProjectGroup) -> some View {
        HStack(spacing: 10) {
            ProjectIcon(projectId: project.projectId, name: project.name, image: fleet.projectIcon(project.projectId),
                        size: 24)
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 6) {
                    Text(project.name).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                        .lineLimit(1)
                    if let state = project.state {
                        StatusGlyph(state: state, size: 7, pulses: false)
                    }
                }
                Text(([project.live.count == 1 ? "1 session" : "\(project.live.count) sessions"] + project.machines)
                    .joined(separator: " · "))
                    .font(.caption).foregroundStyle(Theme.tertiary).lineLimit(1)
            }
            Spacer(minLength: 4)
            if !project.age.isEmpty {
                Text(project.age).font(.caption).foregroundStyle(Theme.tertiary)
            }
            if let id = project.projectId {
                // Borderless, so tapping it starts a session rather than unfolding the row.
                Button("New Session", systemImage: "plus") { draft = Draft.inProject(id, fleet: fleet) }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.borderless)
                    .foregroundStyle(Theme.secondary)
            }
        }
        .contentShape(.rect)
        .contextMenu {
            if let id = project.projectId {
                Button("New Session", systemImage: "plus") { draft = Draft.inProject(id, fleet: fleet) }
                Button("Project Settings", systemImage: "gearshape") { sheet = .projectSettings(projectId: id) }
            }
        }
        .swipeActions(edge: .trailing) {
            if let id = project.projectId {
                Button("Settings", systemImage: "gearshape") { sheet = .projectSettings(projectId: id) }
                    .tint(Theme.raised)
            }
        }
    }

    private func row(_ session: SessionSummary) -> some View {
        NavigationLink(value: session.key) {
            SessionRow(session: session, showsProject: false)
        }
        .listRowBackground(Theme.surface)
        .swipeActions(edge: .trailing) {
            if session.state != .archived {
                Button("Archive", systemImage: "archivebox") {
                    Task { await fleet.archive(session.key) }
                }
                .tint(Theme.raised)
            }
        }
        .contextMenu {
            if session.state != .archived {
                Button("Archive", systemImage: "archivebox") { Task { await fleet.archive(session.key) } }
            }
        }
    }
}
