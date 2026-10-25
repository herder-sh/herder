import Herder
import SwiftUI

/// iPad and the Mac: herder's own sidebar and panes, edge to edge, with no system chrome
/// around them. The Board and projects show their sessions beside the open one;
/// Machines fills the width. An object opens as a page over the section, under a breadcrumb
/// back to it.
struct DesktopShell: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    @Binding var item: SidebarItem
    @Binding var pages: PageStack
    @Binding var session: SessionKey?
    @Binding var draft: Draft?
    let opened: (SessionKey) -> Void
    @AppStorage("sidebarCollapsed") private var sidebarCollapsed = true
    @AppStorage(ProjectOrder.sortKey) private var projectSort = ProjectSort.manual
    @AppStorage(ProjectOrder.manualKey) private var projectOrder = ""
    @State private var query = ""
    /// The open draft's prompt as typed, for its card.
    @State private var typed = ""

    var body: some View {
        HStack(spacing: 0) {
            Group {
                if sidebarCollapsed {
                    SidebarRail(fleet: fleet, item: section, session: $session, collapsed: $sidebarCollapsed,
                                order: order, newSession: newSession)
                        .frame(width: 76)
                } else {
                    Sidebar(fleet: fleet, item: section, session: $session, sheet: $sheet, collapsed: $sidebarCollapsed,
                            sort: $projectSort, manual: $projectOrder, newSession: newSession)
                        .frame(width: 228)
                }
            }
            .background(Theme.surface)
            Rectangle().fill(Theme.stroke).frame(width: 1)
            Group {
                if let page = pages.top {
                    VStack(spacing: 0) {
                        PageTrail(crumbs: crumbs) { pages.back(to: $0) }
                            .overlay(alignment: .trailing) { ActivityChip(fleet: fleet).padding(.trailing, 20) }
                        page.view(fleet: fleet).id(page)
                    }
                } else {
                    content
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .environment(\.pages, PageNavigator(open: { pages.open($0) }, close: { pages.close() }))
        .background(Theme.background)
        #if os(macOS)
        .ignoresSafeArea(.container, edges: .top)
        #endif
    }

    /// The sidebar's selection: choosing a section, even the one shown, closes its pages.
    private var section: Binding<SidebarItem> {
        Binding(get: { item }, set: { next in
            pages.back(to: 0)
            item = next
        })
    }

    /// The breadcrumb: the section, then each page open over it.
    private var crumbs: [String] {
        let lists = fleet.lists
        let root = switch item {
        case .board: "Board"
        case .chats: "Chats"
        case .skills: "Skills"
        case .providers: "Providers"
        case .machines: "Machines"
        case .vault: "Vault"
        case .project(let id): lists.projects.first { $0.id == id }?.name ?? "Project"
        }
        return [root] + pages.pages.map { page in
            // A project's settings opened from elsewhere say whose they are.
            if case .projectSettings(let id) = page, item != .project(id) {
                return "\(lists.projects.first { $0.id == id }?.name ?? "Project") Settings"
            }
            return page.title(in: fleet)
        }
    }

    @ViewBuilder private var content: some View {
        let lists = fleet.lists
        switch item {
        case .project(let id):
            let project = lists.projects.first { $0.id == id }
            ListAndSession(fleet: fleet, session: $session, draft: $draft, typed: $typed, opened: opened) {
                Pane(fleet: fleet, title: project?.name ?? "Project",
                     status: project.map { ProjectSessions.status(live: $0.live, archived: $0.archived) } ?? "",
                     state: project?.state, icon: ProjectIcon(projectId: project?.projectId, name: project?.name, image: fleet.projectIcon(project?.projectId), size: 30),
                     switcher: switcher, query: $query) {
                    drafting({ $0.projectId == id }) {
                        if let project {
                            ProjectSessions(fleet: fleet, live: project.live, archived: project.archived,
                                            query: query, selection: $session)
                        }
                    }
                } actions: {
                    BoardSortMenu(BoardSort.sessions, key: BoardSort.sessionsKey).fixedSize()
                    PaneButton(title: "New Session", symbol: "plus", prominent: true) { draft = Draft.inProject(id, fleet: fleet) }
                    IconButton(symbol: "gearshape", help: "Project Settings") { pages.open(.projectSettings(projectId: id)) }
                }
            }
        case .chats:
            ListAndSession(fleet: fleet, session: $session, draft: $draft, typed: $typed, opened: opened) {
                Pane(fleet: fleet, title: "Chats", switcher: switcher, query: $query) {
                    drafting(\.isChat) {
                        ProjectSessions(fleet: fleet, live: lists.chats, archived: lists.archivedChats,
                                        query: query, selection: $session)
                    }
                } actions: {
                    BoardSortMenu(BoardSort.sessions, key: BoardSort.sessionsKey).fixedSize()
                    let started = Draft.chat(fleet: fleet)
                    PaneButton(title: "New Chat", symbol: "plus", prominent: true) { draft = started }
                        .disabled(started == nil)
                }
            }
        case .board:
            ListAndSession(fleet: fleet, session: $session, draft: $draft, typed: $typed, opened: opened) {
                Pane(fleet: fleet, title: "Board", status: boardStatus(lists), switcher: switcher, query: $query) {
                    drafting { BoardView(fleet: fleet, sheet: $sheet, selection: $session, query: query) }
                } actions: {
                    BoardSortMenu().fixedSize()
                    PaneButton(title: "New Session", symbol: "plus", prominent: true) { sheet = .newSession }
                }
            }
        case .skills:
            Pane(fleet: fleet, title: "Skills", switcher: switcher) {
                SkillsView(fleet: fleet, session: session)
            } actions: {
                EmptyView()
            }
        case .providers:
            Pane(fleet: fleet, title: "Providers", switcher: switcher) {
                ProvidersView(fleet: fleet, sheet: $sheet)
            } actions: {
                PaneButton(title: "Add Account", symbol: "plus", prominent: true) { sheet = .addAccount }
            }
        case .machines:
            Pane(fleet: fleet, title: "Machines", status: connectionStatus(lists), switcher: switcher) {
                MachinesView(fleet: fleet, sheet: $sheet)
            } actions: {
                if !AwsSignIn.machines(fleet.machines).isEmpty {
                    PaneButton(title: "AWS Sign-in", symbol: "key") { sheet = .awsSignIn }
                }
                IconButton(symbol: "qrcode", help: "Pair Another Device") { sheet = .share }
                PaneButton(title: "Add Machine", symbol: "plus", prominent: true) { sheet = .pair }
            }
        case .vault:
            Pane(fleet: fleet, title: "Vault", switcher: switcher) {
                VaultView(fleet: fleet, sheet: $sheet)
            } actions: {
                EmptyView()
            }
        }
    }

    /// A list under the cards of the sessions being written, so they show among the rest until
    /// started or dropped: the open draft, then those left unsent that `shows`, by default
    /// every one but a chat.
    private func drafting<List: View>(
        _ shows: (Draft) -> Bool = { !$0.isChat }, @ViewBuilder _ list: () -> List
    ) -> some View {
        let unsent = PromptDrafts.shared.unsent.compactMap { kept in kept.draft.map { (draft: $0, text: kept.text) } }
            .filter { $0.draft.key != draft?.key && shows($0.draft) }
        let cards = (draft.map { [(draft: $0, text: typed)] } ?? []) + unsent
        return VStack(spacing: 8) {
            ForEach(cards, id: \.draft.key) { card in
                DraftCard(draft: card.draft, fleet: fleet, text: card.text, open: card.draft.key == draft?.key) {
                    draft = card.draft
                } drop: {
                    PromptDrafts.shared.drop(card.draft)
                    if draft?.key == card.draft.key { draft = nil }
                }
                .frame(maxWidth: 760)
                .padding(.horizontal, 16)
            }
            list().padding(.top, cards.isEmpty ? 0 : 2)
        }
    }

    /// ⌘N: a chat in Chats; a draft in the project pane's project, on the machine it would
    /// pick; elsewhere, even beside an open session in a project, the picker.
    private func newSession() {
        if item == .chats, let draft = Draft.chat(fleet: fleet) {
            self.draft = draft
        } else if let projectId = fleet.lists.project(for: item),
           let draft = Draft.inProject(projectId, fleet: fleet) {
            self.draft = draft
        } else {
            sheet = .newSession
        }
    }

    private var order: ProjectOrder { ProjectOrder(sort: projectSort, stored: projectOrder) }

    private var switcher: Switcher { Switcher(fleet: fleet, item: section, session: $session, order: order) }

    /// How many tasks are open and how many of them work, then the machines while one is not
    /// connected.
    private func boardStatus(_ lists: Lists) -> String {
        let tasks = lists.board()
        let working = tasks.filter { $0.state == .working }.count
        let line = "\(tasks.count) open · \(working) working"
        let connected = lists.machines.filter(\.connected).count
        return connected < lists.machines.count || lists.machines.isEmpty ? "\(line) · \(connectionStatus(lists))" : line
    }

    private func connectionStatus(_ lists: Lists) -> String {
        let connected = lists.machines.filter(\.connected).count
        if lists.machines.isEmpty { return "No machines yet" }
        if connected < lists.machines.count { return "\(connected) of \(lists.machines.count) machines connected" }
        return lists.machines.count == 1 ? "1 machine connected" : "All \(lists.machines.count) machines connected"
    }
}

/// A list pane beside the open session; where that is too narrow, the open session takes
/// the list's place, with a way back.
private struct ListAndSession<List: View>: View {
    let fleet: Fleet
    @Binding var session: SessionKey?
    @Binding var draft: Draft?
    @Binding var typed: String
    let opened: (SessionKey) -> Void
    @ViewBuilder var list: List
    @AppStorage("listHidden") private var listHidden = false

    var body: some View {
        GeometryReader { geometry in
            if geometry.size.width >= 820 {
                HStack(spacing: 0) {
                    if !listHidden || (session == nil && draft == nil) {
                        list.frame(width: 380)
                        Rectangle().fill(Theme.stroke).frame(width: 1)
                    }
                    detail.frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            } else if session != nil || draft != nil {
                VStack(alignment: .leading, spacing: 0) {
                    Button { session = nil; draft = nil } label: {
                        Label("Back", systemImage: "chevron.left")
                            .font(.body.weight(.medium))
                            .foregroundStyle(Theme.text)
                            .frame(minHeight: 44)
                            .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .padding(.horizontal, 16)
                    detail
                }
            } else {
                list
            }
        }
    }

    @ViewBuilder private var detail: some View {
        if let draft {
            DraftSessionView(fleet: fleet, draft: draft, created: opened, moved: { self.draft = $0 }, typed: $typed)
                .id(draft.id)
        } else if let session {
            SessionView(fleet: fleet, key: session) { self.session = $0 }
        } else {
            VStack(spacing: 10) {
                Image(systemName: "text.bubble").font(.largeTitle).foregroundStyle(Theme.tertiary)
                Text("Select a session").font(.headline).foregroundStyle(Theme.secondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

/// A pane: its `PageHeader` with the Activity chip, a search field when it has one, then the
/// content.
struct Pane<Content: View, Actions: View>: View {
    /// For the Activity chip, before the page's actions.
    let fleet: Fleet
    let title: String
    /// The page's live status sentence (`PageHeader.status`).
    var status = ""
    /// A project's rolled-up state, before the status.
    var state: SessionState?
    /// A project's tile before the title.
    var icon: ProjectIcon?
    /// Makes the title a menu of the app's sections and projects.
    var switcher: Switcher?
    /// Shows a search field under the header.
    var query: Binding<String>?
    @ViewBuilder var content: Content
    @ViewBuilder var actions: Actions

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            PageHeader(title: title, status: status, state: state, icon: icon, switcher: switcher) {
                ActivityChip(fleet: fleet)
                actions
            }
            if let query {
                HStack(spacing: 8) {
                    Image(systemName: "magnifyingglass").foregroundStyle(Theme.tertiary)
                    TextField("Search sessions, branches, PRs", text: query)
                        .textFieldStyle(.plain)
                        .foregroundStyle(Theme.text)
                    if !query.wrappedValue.isEmpty {
                        Button { query.wrappedValue = "" } label: {
                            Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.tertiary)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .font(.subheadline)
                .padding(.horizontal, 12)
                .frame(height: 34)
                .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
                .padding(.horizontal, 16)
                .padding(.bottom, 8)
            }
            content
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}

/// The sections and projects, for a pane title's menu, so the sidebar can stay collapsed.
struct Switcher {
    /// The symbol of Chats, wherever it is listed.
    static let chatSymbol = "bubble.left.and.bubble.right"
    let fleet: Fleet
    let item: Binding<SidebarItem>
    let session: Binding<SessionKey?>
    /// The projects' order, the sidebar's.
    let order: ProjectOrder

    @MainActor @ViewBuilder var items: some View {
        Button("Board", systemImage: "checklist") { go(.board) }
        Button("Chats", systemImage: Self.chatSymbol) { go(.chats) }
        Divider()
        Button("Providers", systemImage: "person.2") { go(.providers) }
        Button("Skills", systemImage: "book.closed") { go(.skills) }
        Button("Machines", systemImage: "server.rack") { go(.machines) }
        Button("Vault", systemImage: "archivebox") { go(.vault) }
        Divider()
        ForEach(order.sorted(fleet.lists.projects)) { project in
            Button(project.name, systemImage: "shippingbox") { go(.project(project.id)) }
        }
    }

    @MainActor private func go(_ next: SidebarItem) {
        if item.wrappedValue != next { session.wrappedValue = nil }
        item.wrappedValue = next
    }
}

/// A labelled header button in herder's style; just its icon where the label does not fit.
/// `prominent` for the page's main action, in the primary style.
struct PaneButton: View {
    let title: String
    let symbol: String
    var prominent = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            ViewThatFits(in: .horizontal) {
                Label(title, systemImage: symbol)
                    .lineLimit(1)
                    .padding(.horizontal, 12)
                    .fixedSize()
                Image(systemName: symbol)
                    .frame(width: 30)
            }
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(Theme.text)
            .frame(height: 30)
            .primaryFill(in: .rect(cornerRadius: 8), on: prominent)
            .hitTarget()
        }
        .buttonStyle(.plain)
        .help(title)
        .accessibilityLabel(title)
    }
}

/// The sidebar: what the user works on, then the workspace's setup, the projects in the order
/// the user chose, and the machines' health in one line at the bottom.
struct Sidebar: View {
    #if os(macOS)
    static let topBar: CGFloat = 52
    #else
    static let topBar: CGFloat = 44
    #endif
    let fleet: Fleet
    @Binding var item: SidebarItem
    @Binding var session: SessionKey?
    @Binding var sheet: AppSheet?
    @Binding var collapsed: Bool
    @Binding var sort: ProjectSort
    /// The manual order, as `ProjectOrder` keeps it.
    @Binding var manual: String
    let newSession: () -> Void
    @State private var showsQuiet = false
    @Environment(\.pages) private var pages

    private func select(_ next: SidebarItem) {
        if item != next { session = nil }
        item = next
    }

    var body: some View {
        let lists = fleet.lists
        let (shown, quiet) = ProjectOrder(sort: sort, stored: manual).folded(lists.projects, open: item)
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Spacer()
                IconButton(symbol: "sidebar.left", help: "Collapse the sidebar") { collapsed = true }
                    .keyboardShortcut("\\", modifiers: [.command, .shift])
                IconButton(symbol: "arrow.clockwise", help: "Reconnect") { fleet.wake() }
                    .keyboardShortcut("r")
                IconButton(symbol: "square.and.pencil", help: "New Session", action: newSession)
                    .keyboardShortcut("n")
            }
            // Room for the window's traffic lights on the Mac.
            .frame(height: Self.topBar)
            .padding(.horizontal, 10)

            SidebarRow(title: "Board", symbol: "checklist", needsYou: lists.requests.count,
                       selected: item == .board) { select(.board) }
            SidebarRow(title: "Chats", symbol: Switcher.chatSymbol,
                       needsYou: lists.chats.filter { $0.state == .needsYou }.count,
                       working: lists.chats.filter { $0.state == .running }.count,
                       selected: item == .chats) { select(.chats) }

            heading("Workspace")
            SidebarRow(title: "Providers", symbol: "person.2", selected: item == .providers) { select(.providers) }
            SidebarRow(title: "Skills", symbol: "book.closed", selected: item == .skills) { select(.skills) }
            SidebarRow(title: "Machines", symbol: "server.rack", selected: item == .machines) { select(.machines) }
            SidebarRow(title: "Vault", symbol: "archivebox", selected: item == .vault) { select(.vault) }

            heading("Projects") {
                Menu {
                    Picker("Sort Projects", selection: $sort) {
                        ForEach(ProjectSort.allCases, id: \.self) { Text($0.label).tag($0) }
                    }
                    .pickerStyle(.inline)
                } label: {
                    Image(systemName: "arrow.up.arrow.down").font(.caption.weight(.bold)).foregroundStyle(Theme.secondary)
                        .frame(width: 24, height: 24).contentShape(.rect)
                }
                .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
                .help("Sort Projects")
                .accessibilityLabel("Sort Projects")
                Button { sheet = .newProject } label: {
                    Image(systemName: "plus").font(.caption.weight(.bold)).foregroundStyle(Theme.secondary)
                        .frame(width: 24, height: 24).contentShape(.rect)
                }
                .buttonStyle(.plain)
                .help("Add Project")
            }
            ScrollView {
                VStack(spacing: 2) {
                    ForEach(shown) { project in row(project, among: shown + quiet) }
                    if !quiet.isEmpty {
                        Button { showsQuiet.toggle() } label: {
                            HStack(spacing: 10) {
                                Image(systemName: "chevron.down")
                                    .rotationEffect(.degrees(showsQuiet ? 0 : -90))
                                    .frame(width: 20)
                                Text(showsQuiet ? "Fewer" : "\(quiet.count) more")
                                Spacer()
                            }
                            .font(.subheadline)
                            .foregroundStyle(Theme.secondary)
                            .padding(.horizontal, 10)
                            .frame(height: 30)
                            .contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                        .help("Projects quiet for 30 days")
                        if showsQuiet {
                            ForEach(quiet) { project in row(project, among: shown + quiet) }
                        }
                    }
                }
            }
            Spacer(minLength: 0)
            MachinesFooter(machines: lists.machines) { select(.machines) }
                .padding(.horizontal, 6)
                .padding(.vertical, 10)
        }
        .padding(.horizontal, 8)
        .frame(maxHeight: .infinity, alignment: .top)
    }

    private func heading<Actions: View>(
        _ title: String, @ViewBuilder actions: () -> Actions = { EmptyView() }
    ) -> some View {
        HStack(spacing: 2) {
            SectionHeading(title: title)
            Spacer()
            actions()
        }
        .frame(minHeight: 24)
        .padding(.leading, 14)
        .padding(.trailing, 6)
        .padding(.top, 18)
        .padding(.bottom, 4)
    }

    /// A project's row; dragging it onto another puts it there and sorts the projects by hand
    /// from the order shown. `all` is that order, the folded ones included.
    private func row(_ project: ProjectGroup, among all: [ProjectGroup]) -> some View {
        SidebarRow(
            title: project.name, symbol: "shippingbox",
            icon: ProjectIcon(projectId: project.projectId, name: project.name, image: fleet.projectIcon(project.projectId)),
            needsYou: project.needsYou, working: project.working,
            selected: item == .project(project.id),
            settings: project.projectId == nil ? nil : {
                select(.project(project.id))
                pages.open(.projectSettings(projectId: project.id))
            }
        ) { select(.project(project.id)) }
        .draggable(project.id)
        .dropDestination(for: String.self) { ids, _ in
            guard let moved = ids.first, project.projectId != nil else { return false }
            let order = ProjectOrder.move(moved, onto: project.id, in: all.compactMap(\.projectId))
            manual = order.joined(separator: "\n")
            sort = .manual
            return true
        }
    }
}

/// The machines in one line: a dot per machine by its health and how many are not well. It
/// opens a popover with each one's status and a way to its page, over Machines.
struct MachinesFooter: View {
    let machines: [MachineSummary]
    /// Shows only the dots, stacked, for the rail.
    var compact = false
    let showAll: () -> Void
    @State private var open = false
    @Environment(\.pages) private var pages

    /// "4 machines · 1 offline": the count, then those not connected.
    nonisolated static func summary(_ machines: [MachineSummary]) -> String {
        guard !machines.isEmpty else { return "No machines" }
        var parts = [machines.count == 1 ? "1 machine" : "\(machines.count) machines"]
        let offline = machines.filter { if case .disconnected = $0.connection { true } else { false } }.count
        let connecting = machines.filter { $0.connection == .connecting }.count
        if offline > 0 { parts.append("\(offline) offline") }
        if connecting > 0 { parts.append("\(connecting) connecting") }
        return parts.joined(separator: " · ")
    }

    var body: some View {
        let summary = Self.summary(machines)
        Button { open.toggle() } label: {
            Group {
                if compact {
                    VStack(spacing: 8) { dots }
                        .frame(width: 38)
                } else {
                    HStack(spacing: 8) {
                        HStack(spacing: 4) { dots }
                        Text(summary).lineLimit(1)
                        Spacer(minLength: 0)
                    }
                    .font(.footnote)
                    .foregroundStyle(Theme.secondary)
                    .padding(.horizontal, 8)
                    .frame(height: 30)
                }
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .help(summary)
        .accessibilityLabel(summary)
        .accessibilityIdentifier("sidebar-machines")
        .popover(isPresented: $open, arrowEdge: compact ? .trailing : .top) {
            VStack(alignment: .leading, spacing: 2) {
                SectionHeading(title: "Machines").padding(.horizontal, 10).padding(.bottom, 4)
                ForEach(machines) { machine in
                    Button {
                        open = false
                        showAll()
                        pages.open(.machine(machine.hostId))
                    } label: {
                        HStack(spacing: 10) {
                            ConnectionMark(state: machine.connection).frame(width: 12)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(machine.name).foregroundStyle(Theme.text).lineLimit(1).truncationMode(.middle)
                                Text(Self.status(machine)).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                            }
                            Spacer(minLength: 12)
                            Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                        }
                        .padding(.horizontal, 10)
                        .frame(minHeight: 40)
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .help("Open \(machine.name)")
                    .accessibilityIdentifier("sidebar-machine-\(machine.name)")
                }
                Divider().padding(.vertical, 4)
                Button {
                    open = false
                    showAll()
                } label: {
                    Text("All Machines").foregroundStyle(Theme.link).padding(.horizontal, 10).frame(height: 28)
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
            }
            .font(.subheadline)
            .padding(.vertical, 10)
            .frame(width: 260)
            .presentationCompactAdaptation(.popover)
        }
    }

    @ViewBuilder private var dots: some View {
        ForEach(machines) { machine in
            ConnectionMark(state: machine.connection)
        }
    }

    /// A machine's line in the popover: its connection, then what runs on it.
    nonisolated static func status(_ machine: MachineSummary) -> String {
        guard machine.connected else { return machine.connection.label }
        return machine.running > 0 ? "\(machine.running) running" : "Idle"
    }
}

/// The sidebar collapsed to a rail of icons, leaving the room to the session. It lists what the
/// sidebar does, in the same order, the quiet projects folded away.
private struct SidebarRail: View {
    let fleet: Fleet
    @Binding var item: SidebarItem
    @Binding var session: SessionKey?
    @Binding var collapsed: Bool
    let order: ProjectOrder
    let newSession: () -> Void

    var body: some View {
        let lists = fleet.lists
        VStack(spacing: 10) {
            Spacer().frame(height: Sidebar.topBar - 8)
            IconButton(symbol: "sidebar.left", help: "Expand the sidebar") { collapsed = false }
                .keyboardShortcut("\\", modifiers: [.command, .shift])
            IconButton(symbol: "square.and.pencil", help: "New Session", action: newSession)
                .keyboardShortcut("n")
            Rectangle().fill(Theme.stroke).frame(width: 28, height: 1)
            rail("checklist", "Board", .board, needsYou: lists.requests.count)
            rail(Switcher.chatSymbol, "Chats", .chats, needsYou: lists.chats.filter { $0.state == .needsYou }.count,
                 working: lists.chats.filter { $0.state == .running }.count)
            Rectangle().fill(Theme.stroke).frame(width: 28, height: 1)
            rail("person.2", "Providers", .providers)
            rail("book.closed", "Skills", .skills)
            rail("server.rack", "Machines", .machines)
            rail("archivebox", "Vault", .vault)
            Rectangle().fill(Theme.stroke).frame(width: 28, height: 1)
            ScrollView {
                VStack(spacing: 10) {
                    ForEach(order.folded(lists.projects, open: item).shown) { project in
                        rail("shippingbox", project.name, .project(project.id), needsYou: project.needsYou,
                             working: project.working,
                             icon: ProjectIcon(projectId: project.projectId, name: project.name,
                                               image: fleet.projectIcon(project.projectId), size: 22))
                    }
                }
            }
            .scrollIndicators(.never)
            Spacer(minLength: 0)
            MachinesFooter(machines: lists.machines, compact: true) {
                if item != .machines { session = nil }
                item = .machines
            }
        }
        .padding(.bottom, 14)
        .frame(maxHeight: .infinity, alignment: .top)
    }

    /// A section or project's button: an amber dot when something waits for the user, a
    /// running mark when something works.
    private func rail(
        _ symbol: String, _ title: String, _ target: SidebarItem, needsYou: Int = 0, working: Int = 0,
        icon: ProjectIcon? = nil
    ) -> some View {
        let value = [needsYou > 0 ? "\(needsYou) need you" : nil, working > 0 ? "\(working) running" : nil]
            .compactMap(\.self).joined(separator: ", ")
        return Button {
            if item != target { session = nil }
            item = target
        } label: {
            Group {
                if let icon { icon } else { Image(systemName: symbol) }
            }
                .foregroundStyle(item == target ? Theme.text : Theme.secondary)
                .frame(width: 38, height: 34)
                .background(item == target ? Theme.raised : .clear, in: .rect(cornerRadius: 8))
                .overlay(alignment: .topTrailing) {
                    if needsYou > 0 { Circle().fill(Urgency.needsYou.color).frame(width: 8, height: 8).offset(x: -4, y: 4) }
                }
                .overlay(alignment: .bottomTrailing) {
                    if working > 0 {
                        StatusGlyph(state: .running, size: 6, pulses: false)
                            .background(Theme.surface, in: .circle)
                            .offset(x: 2, y: 2)
                    }
                }
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .help(title)
        // Named by its title, not the symbol or the marks it shows.
        .accessibilityLabel(title)
        .accessibilityValue(value)
    }
}

/// A section or project in the sidebar. It shows only what is live: an amber count of what
/// waits for the user, a blue count of what is working, nothing when quiet.
private struct SidebarRow: View {
    let title: String
    let symbol: String
    /// A project's tile, in the symbol's place.
    var icon: ProjectIcon?
    var needsYou = 0
    var working = 0
    let selected: Bool
    /// Opens the row's settings, from a gear shown on hover and when selected.
    var settings: (() -> Void)?
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Group {
                    if let icon { icon } else { Image(systemName: symbol) }
                }
                    .frame(width: 20)
                    .foregroundStyle(selected ? Theme.text : Theme.secondary)
                Text(title).foregroundStyle(Theme.text).lineLimit(1)
                Spacer()
                if let settings, hovering || selected {
                    Button(action: settings) {
                        Image(systemName: "gearshape").foregroundStyle(Theme.secondary)
                            .frame(width: 22, height: 22).contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .help("Project Settings")
                } else {
                    HStack(spacing: 6) {
                        if working > 0 {
                            Text("\(working)")
                                .font(.caption.weight(.semibold).monospacedDigit())
                                .foregroundStyle(Urgency.running.color)
                                .help("\(working) running")
                        }
                        if needsYou > 0 {
                            Text("\(needsYou)")
                                .font(.caption.weight(.semibold).monospacedDigit())
                                .foregroundStyle(Theme.background)
                                .padding(.horizontal, 7)
                                .padding(.vertical, 2)
                                .background(Urgency.needsYou.color, in: .capsule)
                                .help("\(needsYou) need you")
                        }
                    }
                }
            }
            .font(.body.weight(selected ? .semibold : .regular))
            .padding(.horizontal, 10)
            .frame(height: 34)
            .background(selected ? Theme.raised : hovering ? Theme.raised.opacity(0.5) : .clear,
                        in: .rect(cornerRadius: 8))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
    }
}

/// A small square icon button in herder's style.
struct IconButton: View {
    let symbol: String
    let help: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.callout.weight(.semibold))
                .foregroundStyle(Theme.secondary)
                .frame(width: 30, height: 30)
                .background(Theme.raised, in: .rect(cornerRadius: 8))
                .hitTarget()
        }
        .buttonStyle(.plain)
        .help(help)
        .accessibilityLabel(help)
    }
}

/// A project's tasks, or the chats, a search finds, as the Board's rows: sectioned as the sort
/// menu orders them, a section a day by default, a dot on those waiting for you; the archived
/// ones last, folded away until asked for. A row's line names its branch, after its machine
/// while the list's sessions run on more than one.
struct ProjectSessions: View {
    let fleet: Fleet
    let live: [SessionSummary]
    let archived: [SessionSummary]
    let query: String
    /// Where a tapped session opens beside the list; `nil` pushes it.
    var selection: Binding<SessionKey?>?
    @AppStorage(BoardSort.sessionsKey) private var sort = BoardSort.recent
    @State private var showsArchived = false
    @Environment(\.undoManager) private var undoManager

    var body: some View {
        let open = TaskTree.trees(live).filter { $0.matches(query) }
        let archived = TaskTree.trees(self.archived).filter { $0.matches(query) }
        let sections = BoardSection.sections(open, by: sort, now: .now)
        let machines = Self.spansMachines(live + self.archived)
        List {
            if open.isEmpty && archived.isEmpty {
                Text("No sessions match.")
                    .foregroundStyle(Theme.tertiary)
                    .padding(.vertical, 24)
                    .taskListRow()
            }
            ForEach(sections) { section in
                if section.title.isEmpty {
                    ForEach(section.trees) { row($0, machines: machines) }
                } else {
                    Section {
                        ForEach(section.trees) { row($0, machines: machines) }
                    } header: {
                        SectionHeading(title: section.title, count: section.trees.count)
                    }
                }
            }
            if !archived.isEmpty {
                archivedLine(archived.count)
                if showsArchived {
                    ForEach(archived) { row($0, machines: machines) }
                }
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .environment(\.defaultMinListRowHeight, 0)
        #if os(macOS)
        .frame(maxWidth: 760)
        .frame(maxWidth: .infinity)
        #endif
    }

    private func row(_ tree: TaskTree, machines: Bool) -> some View {
        TaskListRow(tree: tree, fleet: fleet, selection: selection,
                    context: Self.context(tree.lead, machines: machines)) {
            tree.toggleArchived(fleet: fleet, undoManager: undoManager)
        }
    }

    /// "Archived · N", which shows or hides the archived tasks.
    private func archivedLine(_ count: Int) -> some View {
        Button {
            withAnimation(.snappy) { showsArchived.toggle() }
        } label: {
            HStack(spacing: 6) {
                Text("Archived · \(count)")
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.bold))
                    .rotationEffect(.degrees(showsArchived ? 90 : 0))
                Spacer()
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .font(.caption)
        .foregroundStyle(Theme.tertiary)
        .padding(.vertical, 10)
        .padding(.horizontal, 8)
        .accessibilityLabel(showsArchived ? "Hide Archived" : "Show Archived")
        .taskListRow()
    }

    /// Whether `sessions` run on more than one machine, so their rows name theirs.
    nonisolated static func spansMachines(_ sessions: [SessionSummary]) -> Bool {
        Set(sessions.map(\.machine)).count > 1
    }

    /// What a row's line says before what its task is doing: its machine when `machines`, then
    /// its branch.
    nonisolated static func context(_ session: SessionSummary, machines: Bool) -> String {
        [machines ? session.machine : "", session.branch].filter { !$0.isEmpty }.joined(separator: " · ")
    }

    /// The project header's live status: how many of its tasks wait for you and work, then how
    /// many it has, archived ones too.
    nonisolated static func status(live: [SessionSummary], archived: [SessionSummary]) -> String {
        let open = TaskTree.trees(live)
        let total = open.count + TaskTree.trees(archived).count
        let waiting = open.filter { $0.state == .waitingForYou }.count
        let working = open.filter { $0.state == .working }.count
        return [
            waiting > 0 ? "\(waiting) waiting for you" : "",
            working > 0 ? "\(working) working" : "",
            total == 1 ? "1 session" : "\(total) sessions",
        ].filter { !$0.isEmpty }.joined(separator: " · ")
    }
}
