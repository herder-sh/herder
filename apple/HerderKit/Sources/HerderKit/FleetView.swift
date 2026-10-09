import Herder
import SwiftUI

/// The sidebar's entries.
enum SidebarItem: Hashable {
    case board, pullRequests, usage, skills, machines, vault
    case project(String)
}

/// The tab bar's tabs on compact width: the sidebar's sections, with the projects as one tab,
/// Pull Requests inside the Board, and Skills and a vault inside Machines.
enum CompactTab: Hashable, CaseIterable {
    case board, projects, usage, machines

    /// The tab that shows a sidebar entry.
    init(_ item: SidebarItem) {
        switch item {
        case .project: self = .projects
        case .board, .pullRequests: self = .board
        case .usage: self = .usage
        case .skills, .machines, .vault: self = .machines
        }
    }

    var title: String {
        switch self {
        case .board: "Board"
        case .projects: "Projects"
        case .usage: "Usage"
        case .machines: "Machines"
        }
    }

    var symbol: String {
        switch self {
        case .board: "checklist"
        case .projects: "square.stack.3d.up"
        case .usage: "chart.bar"
        case .machines: "server.rack"
        }
    }
}

/// The fleet: tabs on iPhone; herder's own sidebar and panes on iPad and the Mac.
struct FleetView: View {
    let fleet: Fleet
    @State private var sheet: AppSheet?
    @State private var item: SidebarItem = .board
    @State private var session: SessionKey?
    @State private var draft: Draft?
    @State private var tab = CompactTab.board
    @State private var projectsPath: [NavRoute] = []
    @State private var boardPath: [NavRoute] = []
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
            sheet.view(fleet: fleet) { selection in
                if sheet == .newProject, let projectId = selection.projectId {
                    draft = nil
                    session = nil
                    item = .project(projectId)
                    tab = .projects
                    projectsPath = [.project(projectId)]
                } else {
                    draft = selection
                }
            }
        }
        #if os(iOS)
        .fullScreenCover(item: Binding(get: { sizeClass == .compact ? draft : nil }, set: { draft = $0 })) { draft in
            NavigationStack {
                DraftSessionView(fleet: fleet, draft: draft, created: opened) { self.draft = $0 }
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
        .alert("Rename Session", isPresented: Binding(get: { fleet.renaming != nil }, set: { if !$0 { fleet.renaming = nil } })) {
            RenameSessionField(title: fleet.renaming.flatMap { fleet.sessions[$0]?.title } ?? "") { title in
                guard let key = fleet.renaming else { return }
                Task { await fleet.rename(key, to: title) }
            }
            Button("Cancel", role: .cancel) {}
        }
        .onChange(of: draft) {
            // A draft shows in a list's session pane; Machines has none. The Board and Pull
            // Requests keep their list, so starting a session there stays there.
            if let draft {
                session = nil
                if showsEverySession { return }
                // The list beside the chat is the draft's project, or the Board for a path.
                let shown = draft.projectId.map { id in fleet.lists.projects.contains { $0.id == id } } ?? false
                item = shown ? .project(draft.projectId ?? "") : .board
            }
        }
        .onChange(of: session) { if session != nil { draft = nil } }
        // An archived session closes, wherever it was open.
        .onChange(of: fleet.archiving) { before, _ in
            for key in fleet.archived(since: before) { close(key) }
        }
        // A removed project's pane has nothing left to show.
        .onChange(of: fleet.lists.projects.map(\.id)) { _, ids in
            if case .project(let id) = item, !ids.contains(id) { item = .board }
        }
        // A draft belongs to the Board, Pull Requests or its own project; leaving for elsewhere
        // drops it.
        .onChange(of: item) {
            guard let draft else { return }
            if item != .project(draft.projectId ?? "") && !showsEverySession { self.draft = nil }
        }
    }

    /// The Board and Pull Requests list every project's sessions, so a session started from
    /// one opens beside it rather than in its project's pane.
    private var showsEverySession: Bool { item == .board || item == .pullRequests }

    private var shell: some View {
        DesktopShell(fleet: fleet, sheet: $sheet, item: $item, session: $session, draft: $draft, opened: opened)
    }

    /// Shows a session just created from a draft: beside the Board or Pull Requests it was
    /// started from, in its project's pane, or pushed on the Projects tab it was started from,
    /// else on the Board.
    private func opened(_ key: SessionKey) {
        draft = nil
        if !showsEverySession, let projectId = fleet.lists.projects.first(where: { $0.sessions.contains { $0.key == key } })?.projectId {
            item = .project(projectId)
        }
        session = key
        if tab == .projects {
            projectsPath.append(.session(key))
        } else {
            tab = .board
            boardPath = [.session(key)]
        }
    }

    /// Closes a session's pane, and pops it and what was pushed over it on every tab.
    private func close(_ key: SessionKey) {
        if session == key { session = nil }
        for path in [$projectsPath, $boardPath] {
            if let index = path.wrappedValue.firstIndex(of: .session(key)) { path.wrappedValue.removeSubrange(index...) }
        }
    }

    #if os(iOS)
    private var tabs: some View {
        TabView(selection: $tab) {
            ForEach(CompactTab.allCases, id: \.self) { tab in
                self.tab(tab)
                    .tabItem { Label(tab.title, systemImage: tab.symbol) }
                    .badge(tab == .board ? fleet.lists.requests.count : 0)
                    .tag(tab)
            }
        }
    }

    @ViewBuilder private func tab(_ tab: CompactTab) -> some View {
        switch tab {
        case .projects:
            NavigationStack(path: $projectsPath) {
                ProjectsView(fleet: fleet, draft: $draft, projects: fleet.lists.projects)
                    .toolbar { Button("Add Project", systemImage: "plus") { sheet = .newProject } }
                    .navigationDestination(for: NavRoute.self, destination: destination)
            }
            .environment(\.sessionPath, $projectsPath)
        case .board:
            NavigationStack(path: $boardPath) {
                BoardView(fleet: fleet, sheet: $sheet, showsPullRequests: true)
                    .toolbar { Button("New Session", systemImage: "plus") { sheet = .newSession } }
                    .navigationDestination(for: NavRoute.self, destination: destination)
            }
            .environment(\.sessionPath, $boardPath)
        case .usage:
            NavigationStack { UsageView(fleet: fleet) }
        case .machines:
            NavigationStack { MachinesView(fleet: fleet, sheet: $sheet, showsVaults: true, showsSkills: true) }
        }
    }

    @ViewBuilder private func destination(_ route: NavRoute) -> some View {
        switch route {
        case .session(let key): SessionView(fleet: fleet, key: key)
        case .project(let id): ProjectView(fleet: fleet, id: id, sheet: $sheet, draft: $draft)
        }
    }
    #endif
}

/// "2 of 3 machines connected".
struct ConnectionLine: View {
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

struct EmptyFleet: View {
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
/// with archiving at hand: a button on hover, and in the context menu beside renaming.
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
                NavigationLink(value: NavRoute.session(session.key)) { row }
                    .buttonStyle(.plain)
            }
        }
        .opacity(fleet.archiving.contains(session.key) ? 0.5 : session.state == .archived ? 0.75 : 1)
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
            if session.state.renamable {
                Button("Rename…", systemImage: "pencil") { fleet.renaming = session.key }
            }
            if session.state != .archived {
                Button("Archive", systemImage: "archivebox") { Task { await fleet.archive(session.key) } }
            }
        }
    }

    private var row: some View {
        SessionRow(session: session, showsProject: showsProject, projectIcon: fleet.projectIcon(session.projectId))
            .padding(.horizontal, 8)
    }
}

/// The projects as compact rows, most recently active first, each opening its own screen and
/// starting a session from its plus. A search keeps the projects whose name or path matches,
/// and those with sessions that match, listing those sessions under it.
struct ProjectsView: View {
    let fleet: Fleet
    @Binding var draft: Draft?
    let projects: [ProjectGroup]
    @State private var query = ""

    private var searching: Bool { !query.trimmingCharacters(in: .whitespaces).isEmpty }

    var body: some View {
        let shown = ProjectGroup.found(projects, query: query)
        List {
            ForEach(shown) { project in
                NavigationLink(value: NavRoute.project(project.id)) { header(project) }
                    .listRowBackground(Theme.surface)
                if searching {
                    ForEach(project.sessions) { row($0) }
                }
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
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .refreshable { fleet.wake() }
        .navigationTitle("Projects")
    }

    /// A project's one compact row: its icon and name, its live sessions and machines, how
    /// long since anything happened in it, and a plus that starts a session in it.
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
                let count = project.live.filter { $0.state != .archived }.count
                Text(([count == 1 ? "1 session" : "\(count) sessions"] + project.machines)
                    .joined(separator: " · "))
                    .font(.caption).foregroundStyle(Theme.tertiary).lineLimit(1)
            }
            Spacer(minLength: 4)
            if !project.age.isEmpty {
                Text(project.age).font(.caption).foregroundStyle(Theme.tertiary)
            }
            if let id = project.projectId {
                let started = Draft.inProject(id, fleet: fleet)
                // Borderless, so tapping it starts a session rather than opening the project.
                Button("New Session", systemImage: "plus") { draft = started }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.borderless)
                    .foregroundStyle(Theme.secondary)
                    .disabled(started == nil)
            }
        }
        .contentShape(.rect)
    }

    private func row(_ session: SessionSummary) -> some View {
        NavigationLink(value: NavRoute.session(session.key)) {
            SessionRow(session: session, showsProject: false)
        }
        .listRowBackground(Theme.surface)
        .padding(.leading, 24)
    }
}

/// A project's own screen on iPhone, as the project's pane on iPad: its sessions, with starting
/// one and the project's settings at hand.
struct ProjectView: View {
    let fleet: Fleet
    let id: String
    @Binding var sheet: AppSheet?
    @Binding var draft: Draft?

    var body: some View {
        let project = fleet.lists.projects.first { $0.id == id }
        ScrollView {
            if let project {
                ProjectSessions(fleet: fleet, live: project.live, archived: project.archived)
                    .padding(16)
            }
        }
        .background(Theme.background)
        .refreshable { fleet.wake() }
        .navigationTitle(project?.name ?? "Project")
        .toolbar {
            if let projectId = project?.projectId {
                Button("Project Settings", systemImage: "gearshape") { sheet = .projectSettings(projectId: id) }
                let started = Draft.inProject(projectId, fleet: fleet)
                Button("New Session", systemImage: "plus") { draft = started }
                    .disabled(started == nil)
            }
        }
    }
}

/// The field of the rename alert, starting from the session's current title.
private struct RenameSessionField: View {
    let title: String
    let rename: (String) -> Void
    @State private var typed = ""

    var body: some View {
        TextField("Title", text: $typed)
            .onAppear { typed = title }
        Button("Rename") {
            let trimmed = typed.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty && trimmed != title { rename(trimmed) }
        }
    }
}
