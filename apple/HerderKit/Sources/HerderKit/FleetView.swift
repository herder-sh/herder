import Herder
import SwiftUI

/// The sidebar's entries.
enum SidebarItem: Hashable {
    case home, projects, machines
    case project(String)
}

/// The fleet: tabs on iPhone, a sidebar with a session list beside it on iPad and the Mac.
struct FleetView: View {
    let fleet: Fleet
    @State private var pairing = false
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    var body: some View {
        Group {
            #if os(iOS)
            if sizeClass == .compact { tabs } else { split }
            #else
            split
            #endif
        }
        .tint(Theme.text)
        .sheet(isPresented: $pairing) { PairSheet(fleet: fleet) }
        .task { await fleet.follow() }
    }

    #if os(iOS)
    private var tabs: some View {
        TabView {
            NavigationStack { HomeView(fleet: fleet, pairing: $pairing) }
                .tabItem { Label("Home", systemImage: "tray.full") }
                .badge(fleet.lists.requests.count)
            NavigationStack { ProjectsView(fleet: fleet, projects: fleet.lists.projects) }
                .tabItem { Label("Projects", systemImage: "square.stack.3d.up") }
            NavigationStack { MachinesView(fleet: fleet, pairing: $pairing) }
                .tabItem { Label("Machines", systemImage: "server.rack") }
        }
    }
    #endif

    @State private var section: SidebarItem? = .home

    private var split: some View {
        NavigationSplitView {
            List(selection: $section) {
                Label("Home", systemImage: "tray.full")
                    .badge(fleet.lists.requests.count)
                    .tag(SidebarItem.home)
                Label("Machines", systemImage: "server.rack").tag(SidebarItem.machines)
                Section("Projects") {
                    ForEach(fleet.lists.projects) { project in
                        Label(project.name, systemImage: "shippingbox")
                            .badge(project.sessions.count)
                            .tag(SidebarItem.project(project.id))
                    }
                }
            }
            .navigationSplitViewColumnWidth(min: 200, ideal: 230)
            .toolbar {
                Button("Add Machine", systemImage: "plus") { pairing = true }
                Button("Reconnect", systemImage: "arrow.clockwise") { fleet.wake() }
                    .keyboardShortcut("r")
            }
        } detail: {
            NavigationStack {
                switch section ?? .home {
                case .home: HomeView(fleet: fleet, pairing: $pairing)
                case .projects: ProjectsView(fleet: fleet, projects: fleet.lists.projects)
                case .machines: MachinesView(fleet: fleet, pairing: $pairing)
                case .project(let id):
                    let projects = fleet.lists.projects.filter { $0.id == id }
                    ProjectsView(fleet: fleet, projects: projects, title: projects.first?.name ?? "Project")
                }
            }
        }
    }
}

/// What needs you, then what is running, then what finished.
struct HomeView: View {
    let fleet: Fleet
    @Binding var pairing: Bool

    var body: some View {
        let lists = fleet.lists
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 22) {
                ConnectionLine(machines: lists.machines)
                if lists.machines.isEmpty {
                    EmptyFleet(pairing: $pairing)
                }
                if !lists.requests.isEmpty {
                    VStack(alignment: .leading, spacing: 10) {
                        SectionHeading(title: "Needs you", count: lists.requests.count, tint: Theme.accent)
                        ForEach(lists.requests) { RequestCard(request: $0, fleet: fleet) }
                    }
                }
                SessionGroup(title: "Active", sessions: lists.active, fleet: fleet)
                SessionGroup(title: "Recent", sessions: Array(lists.recent.prefix(20)), fleet: fleet)
            }
            .frame(maxWidth: 760)
            .frame(maxWidth: .infinity)
            .padding(.horizontal, 16)
            .padding(.bottom, 24)
        }
        .background(Theme.background)
        .refreshable { fleet.wake() }
        .navigationTitle("herder")
        .navigationDestination(for: SessionKey.self) { SessionPlaceholder(fleet: fleet, key: $0) }
    }
}

/// "2 of 3 machines connected".
private struct ConnectionLine: View {
    let machines: [MachineSummary]

    var body: some View {
        let connected = machines.filter(\.connected).count
        if !machines.isEmpty {
            HStack(spacing: 8) {
                Circle().fill(connected == machines.count ? Theme.success : Theme.accent)
                    .frame(width: 7, height: 7)
                Text(connected == machines.count
                     ? (machines.count == 1 ? "1 machine connected" : "All \(machines.count) machines connected")
                     : "\(connected) of \(machines.count) machines connected")
            }
            .font(.footnote.weight(.medium))
            .foregroundStyle(Theme.secondary)
        }
    }
}

private struct EmptyFleet: View {
    @Binding var pairing: Bool

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 12) {
                Text("No machines yet").font(.headline).foregroundStyle(Theme.text)
                Text("Run `herder pair` on a machine, then add it here with the link it prints.")
                    .font(.subheadline)
                    .foregroundStyle(Theme.secondary)
                ActionButton(title: "Add Machine", style: .primary) { pairing = true }
                    #if os(macOS)
                    .frame(maxWidth: 220)
                    #endif
            }
        }
    }
}

/// A titled card of session rows.
private struct SessionGroup: View {
    let title: String
    let sessions: [SessionSummary]
    let fleet: Fleet

    var body: some View {
        if !sessions.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                SectionHeading(title: title, count: sessions.count)
                VStack(spacing: 0) {
                    ForEach(Array(sessions.enumerated()), id: \.element.id) { index, session in
                        if index > 0 { Divider().overlay(Theme.stroke).padding(.leading, 30) }
                        NavigationLink(value: session.key) {
                            SessionRow(session: session)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 12)
                .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
            }
        }
    }
}

/// Every session, grouped by project, with the task tree.
struct ProjectsView: View {
    let fleet: Fleet
    let projects: [ProjectGroup]
    var title = "Projects"

    var body: some View {
        List {
            ForEach(projects) { project in
                Section {
                    ForEach(project.sessions) { session in
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
                    }
                } header: {
                    HStack(spacing: 6) {
                        Image(systemName: project.projectId == nil ? "questionmark.folder" : "shippingbox")
                            .foregroundStyle(Theme.secondary)
                        Text(project.name).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                        Text(project.machines.joined(separator: ", ")).font(.caption).foregroundStyle(Theme.tertiary)
                    }
                    .textCase(nil)
                }
            }
        }
        .overlay {
            if projects.isEmpty {
                ContentUnavailableView("No sessions", systemImage: "square.stack.3d.up",
                                       description: Text("Sessions on your machines appear here."))
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .refreshable { fleet.wake() }
        .navigationTitle(title)
        .navigationDestination(for: SessionKey.self) { SessionPlaceholder(fleet: fleet, key: $0) }
    }
}

/// Holds the session view's place until P7.3.
struct SessionPlaceholder: View {
    let fleet: Fleet
    let key: SessionKey

    var body: some View {
        let session = fleet.sessions[key]
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                StatusGlyph(state: session?.state ?? .idle)
                Text(session?.state.label ?? "").foregroundStyle(Theme.secondary)
            }
            Text(session?.activity ?? "").foregroundStyle(Theme.text)
            Text(session?.branch ?? "").font(Theme.monoSmall).foregroundStyle(Theme.tertiary)
            Spacer()
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.background)
        .navigationTitle(session?.title ?? "")
    }
}
