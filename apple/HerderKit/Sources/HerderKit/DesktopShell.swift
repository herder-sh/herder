import Herder
import SwiftUI

/// iPad and the Mac: herder's own sidebar and panes, edge to edge, with no system chrome
/// around them. Home and projects show their sessions beside the open one; Machines fills
/// the width.
struct DesktopShell: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    @Binding var item: SidebarItem
    @Binding var session: SessionKey?
    @Binding var draft: Draft?
    let opened: (SessionKey) -> Void
    @AppStorage("sidebarCollapsed") private var sidebarCollapsed = false

    var body: some View {
        HStack(spacing: 0) {
            Group {
                if sidebarCollapsed {
                    SidebarRail(fleet: fleet, item: $item, session: $session, sheet: $sheet, collapsed: $sidebarCollapsed)
                        .frame(width: 76)
                } else {
                    Sidebar(fleet: fleet, item: $item, session: $session, sheet: $sheet, collapsed: $sidebarCollapsed)
                        .frame(width: 228)
                }
            }
            .background(Theme.surface)
            Rectangle().fill(Theme.stroke).frame(width: 1)
            content
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .background(Theme.background)
        #if os(macOS)
        .ignoresSafeArea(.container, edges: .top)
        #endif
    }

    @ViewBuilder private var content: some View {
        let lists = fleet.lists
        switch item {
        case .home:
            ListAndSession(fleet: fleet, session: $session, draft: $draft, opened: opened) {
                Pane(title: "Home", subtitle: subtitle(lists)) {
                    HomeView(fleet: fleet, sheet: $sheet, selection: $session)
                } actions: {
                    PaneButton(title: "New Session", symbol: "plus") { sheet = .newSession }
                }
            }
        case .project(let id):
            let project = lists.projects.first { $0.id == id }
            ListAndSession(fleet: fleet, session: $session, draft: $draft, opened: opened) {
                Pane(title: project?.name ?? "Project", subtitle: project?.machines.joined(separator: ", ") ?? "") {
                    ScrollView {
                        if let project {
                            SessionGroup(title: "Sessions", sessions: project.sessions, fleet: fleet,
                                         selection: $session, showsProject: false)
                                .padding(16)
                        }
                    }
                } actions: {
                    PaneButton(title: "New Session", symbol: "plus") { draft = Draft.inProject(id, fleet: fleet) }
                    IconButton(symbol: "gearshape", help: "Project Settings") { sheet = .projectSettings(projectId: id) }
                }
            }
        case .pullRequests:
            ListAndSession(fleet: fleet, session: $session, draft: $draft, opened: opened) {
                Pane(title: "Pull Requests", subtitle: "Linked to sessions") {
                    PullRequestsView(fleet: fleet, selection: $session)
                } actions: {
                    EmptyView()
                }
            }
        case .machines:
            Pane(title: "Machines", subtitle: subtitle(lists)) {
                MachinesView(fleet: fleet, sheet: $sheet)
            } actions: {
                PaneButton(title: "Add Machine", symbol: "plus") { sheet = .pair }
            }
        }
    }

    private func subtitle(_ lists: Lists) -> String {
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
                        .overlay(alignment: .topTrailing) {
                            if session != nil || draft != nil {
                                IconButton(symbol: listHidden ? "sidebar.squares.left" : "arrow.up.left.and.arrow.down.right",
                                           help: listHidden ? "Show the session list" : "Hide the session list") {
                                    listHidden.toggle()
                                }
                                .keyboardShortcut("\\", modifiers: .command)
                                .padding(.top, 14)
                                .padding(.trailing, 64)
                            }
                        }
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
            DraftSessionView(fleet: fleet, draft: draft, created: opened).id(draft.id)
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

/// A pane with herder's header: a title, a quiet subtitle, the pane's actions, then the content.
struct Pane<Content: View, Actions: View>: View {
    let title: String
    var subtitle = ""
    @ViewBuilder var content: Content
    @ViewBuilder var actions: Actions

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .center, spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.title2.weight(.bold)).foregroundStyle(Theme.text).lineLimit(1)
                    if !subtitle.isEmpty {
                        Text(subtitle).font(.footnote).foregroundStyle(Theme.secondary).lineLimit(1)
                    }
                }
                Spacer()
                actions
            }
            .padding(.horizontal, 20)
            .padding(.top, 22)
            .padding(.bottom, 10)
            content
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}

/// A labelled header button in herder's style.
struct PaneButton: View {
    let title: String
    let symbol: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Label(title, systemImage: symbol)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Theme.onPrimary)
                .padding(.horizontal, 12)
                .frame(height: 30)
                .background(Theme.primary, in: .rect(cornerRadius: 8))
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }
}

/// The sidebar: sections, projects, and the machines' status at the bottom.
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

    private func select(_ next: SidebarItem) {
        if item != next { session = nil }
        item = next
    }

    var body: some View {
        let lists = fleet.lists
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Spacer()
                IconButton(symbol: "sidebar.left", help: "Collapse the sidebar") { collapsed = true }
                    .keyboardShortcut("\\", modifiers: [.command, .shift])
                IconButton(symbol: "arrow.clockwise", help: "Reconnect") { fleet.wake() }
                    .keyboardShortcut("r")
                IconButton(symbol: "square.and.pencil", help: "New Session") { sheet = .newSession }
                    .keyboardShortcut("n")
            }
            // Room for the window's traffic lights on the Mac.
            .frame(height: Self.topBar)
            .padding(.horizontal, 10)

            SidebarRow(title: "Home", symbol: "tray.full", badge: lists.requests.count, attention: true,
                       selected: item == .home) { select(.home) }
            SidebarRow(title: "Pull Requests", symbol: "arrow.triangle.pull",
                       badge: lists.pullRequests(openOnly: true).flatMap(\.sessions).map(\.prs.count).reduce(0, +),
                       selected: item == .pullRequests) { select(.pullRequests) }
            SidebarRow(title: "Machines", symbol: "server.rack", badge: lists.machines.count,
                       selected: item == .machines) { select(.machines) }

            HStack {
                SectionHeading(title: "Projects")
                Spacer()
                Button { sheet = .newProject } label: {
                    Image(systemName: "plus").font(.caption.weight(.bold)).foregroundStyle(Theme.secondary)
                        .frame(width: 24, height: 24).contentShape(.rect)
                }
                .buttonStyle(.plain)
                .help("New Project")
            }
            .padding(.leading, 14)
            .padding(.trailing, 6)
            .padding(.top, 18)
            .padding(.bottom, 4)
            ScrollView {
                VStack(spacing: 2) {
                    ForEach(lists.projects) { project in
                        SidebarRow(
                            title: project.name, symbol: "shippingbox",
                            badge: project.sessions.count,
                            selected: item == .project(project.id),
                            settings: project.projectId == nil ? nil : { sheet = .projectSettings(projectId: project.id) }
                        ) { select(.project(project.id)) }
                    }
                }
            }
            Spacer(minLength: 0)
            VStack(alignment: .leading, spacing: 8) {
                ForEach(lists.machines) { machine in
                    Button { sheet = .machineSettings(hostId: machine.hostId) } label: {
                        HStack(spacing: 8) {
                            ConnectionMark(state: machine.connection)
                            Text(machine.name).lineLimit(1).truncationMode(.middle)
                            Spacer()
                            if machine.running > 0 {
                                Text("\(machine.running)").foregroundStyle(Theme.running)
                            }
                        }
                        .font(.footnote)
                        .foregroundStyle(Theme.secondary)
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .help("\(machine.name): \(machine.connection.label)")
                }
            }
            .padding(14)
        }
        .padding(.horizontal, 8)
        .frame(maxHeight: .infinity, alignment: .top)
    }
}

/// The sidebar collapsed to a rail of icons, leaving the room to the session.
private struct SidebarRail: View {
    let fleet: Fleet
    @Binding var item: SidebarItem
    @Binding var session: SessionKey?
    @Binding var sheet: AppSheet?
    @Binding var collapsed: Bool

    var body: some View {
        let lists = fleet.lists
        VStack(spacing: 10) {
            Spacer().frame(height: Sidebar.topBar - 8)
            IconButton(symbol: "sidebar.left", help: "Expand the sidebar") { collapsed = false }
                .keyboardShortcut("\\", modifiers: [.command, .shift])
            IconButton(symbol: "square.and.pencil", help: "New Session") { sheet = .newSession }
                .keyboardShortcut("n")
            Rectangle().fill(Theme.stroke).frame(width: 28, height: 1)
            rail("tray.full", "Home", .home, badge: lists.requests.count)
            rail("arrow.triangle.pull", "Pull Requests", .pullRequests, badge: 0)
            rail("server.rack", "Machines", .machines, badge: 0)
            ForEach(lists.projects) { project in
                rail("shippingbox", project.name, .project(project.id), badge: 0)
            }
            Spacer()
            ForEach(lists.machines) { machine in
                ConnectionMark(state: machine.connection).help("\(machine.name): \(machine.connection.label)")
            }
        }
        .padding(.bottom, 14)
        .frame(maxHeight: .infinity, alignment: .top)
    }

    private func rail(_ symbol: String, _ title: String, _ target: SidebarItem, badge: Int) -> some View {
        Button {
            if item != target { session = nil }
            item = target
        } label: {
            Image(systemName: symbol)
                .foregroundStyle(item == target ? Theme.text : Theme.secondary)
                .frame(width: 38, height: 34)
                .background(item == target ? Theme.raised : .clear, in: .rect(cornerRadius: 8))
                .overlay(alignment: .topTrailing) {
                    if badge > 0 { Circle().fill(Theme.accent).frame(width: 8, height: 8).offset(x: -4, y: 4) }
                }
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .help(title)
    }
}

private struct SidebarRow: View {
    let title: String
    let symbol: String
    var badge = 0
    var attention = false
    let selected: Bool
    /// Opens the row's settings, from a gear shown on hover and when selected.
    var settings: (() -> Void)?
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: symbol)
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
                } else if badge > 0 {
                    Text("\(badge)")
                        .font(.caption.weight(.semibold).monospacedDigit())
                        .foregroundStyle(attention ? Theme.onPrimary : Theme.tertiary)
                        .padding(.horizontal, attention ? 7 : 0)
                        .padding(.vertical, attention ? 2 : 0)
                        .background(attention ? Theme.accent : .clear, in: .capsule)
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
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .help(help)
        .accessibilityLabel(help)
    }
}
