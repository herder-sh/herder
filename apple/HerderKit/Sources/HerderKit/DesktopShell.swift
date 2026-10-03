import Herder
import SwiftUI

/// iPad and the Mac: herder's own sidebar and panes, edge to edge, with no system chrome
/// around them. Home and projects show their sessions beside the open one; Machines fills
/// the width.
struct DesktopShell: View {
    let fleet: Fleet
    @Binding var pairing: Bool
    @State private var item: SidebarItem = .home
    @State private var session: SessionKey?

    var body: some View {
        HStack(spacing: 0) {
            Sidebar(fleet: fleet, item: $item, pairing: $pairing)
                .frame(width: 236)
            Rectangle().fill(Theme.stroke).frame(width: 1)
            content
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .background(Theme.background)
        .ignoresSafeArea(.container, edges: .top)
        .onChange(of: item) { session = nil }
    }

    @ViewBuilder private var content: some View {
        let lists = fleet.lists
        switch item {
        case .home:
            ListAndSession(fleet: fleet, session: $session) {
                Pane(title: "Home", subtitle: subtitle(lists)) {
                    HomeView(fleet: fleet, pairing: $pairing, selection: $session)
                }
            }
        case .project(let id):
            let project = lists.projects.first { $0.id == id }
            ListAndSession(fleet: fleet, session: $session) {
                Pane(title: project?.name ?? "Project", subtitle: project?.machines.joined(separator: ", ") ?? "") {
                    ScrollView {
                        if let project {
                            SessionGroup(title: "Sessions", sessions: project.sessions, selection: $session,
                                         showsProject: false)
                                .padding(16)
                        }
                    }
                }
            }
        case .machines:
            Pane(title: "Machines", subtitle: subtitle(lists)) {
                MachinesView(fleet: fleet, pairing: $pairing)
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

/// A list pane beside the open session.
private struct ListAndSession<List: View>: View {
    let fleet: Fleet
    @Binding var session: SessionKey?
    @ViewBuilder var list: List

    var body: some View {
        HStack(spacing: 0) {
            list.frame(width: 440)
            Rectangle().fill(Theme.stroke).frame(width: 1)
            Group {
                if let session {
                    SessionPlaceholder(fleet: fleet, key: session)
                } else {
                    VStack(spacing: 10) {
                        Image(systemName: "text.bubble").font(.largeTitle).foregroundStyle(Theme.tertiary)
                        Text("Select a session").font(.headline).foregroundStyle(Theme.secondary)
                    }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

/// A pane with herder's header: a title, a quiet subtitle, then the content.
struct Pane<Content: View>: View {
    let title: String
    var subtitle = ""
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.title2.weight(.bold)).foregroundStyle(Theme.text)
                if !subtitle.isEmpty {
                    Text(subtitle).font(.footnote).foregroundStyle(Theme.secondary)
                }
            }
            .padding(.horizontal, 20)
            .padding(.top, 22)
            .padding(.bottom, 10)
            content
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}

/// The sidebar: sections, projects, and the machines' status at the bottom.
private struct Sidebar: View {
    let fleet: Fleet
    @Binding var item: SidebarItem
    @Binding var pairing: Bool

    var body: some View {
        let lists = fleet.lists
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Spacer()
                IconButton(symbol: "arrow.clockwise", help: "Reconnect") { fleet.wake() }
                    .keyboardShortcut("r")
                IconButton(symbol: "plus", help: "Add Machine") { pairing = true }
            }
            // Room for the window's traffic lights on the Mac.
            .frame(height: 52)
            .padding(.horizontal, 10)

            SidebarRow(title: "Home", symbol: "tray.full", badge: lists.requests.count, attention: true,
                       selected: item == .home) { item = .home }
            SidebarRow(title: "Machines", symbol: "server.rack", selected: item == .machines) { item = .machines }

            SectionHeading(title: "Projects")
                .padding(.horizontal, 14)
                .padding(.top, 18)
                .padding(.bottom, 6)
            ScrollView {
                VStack(spacing: 2) {
                    ForEach(lists.projects) { project in
                        SidebarRow(
                            title: project.name, symbol: "shippingbox",
                            badge: project.sessions.count,
                            selected: item == .project(project.id)
                        ) { item = .project(project.id) }
                    }
                }
            }
            Spacer(minLength: 0)
            VStack(alignment: .leading, spacing: 8) {
                ForEach(lists.machines) { machine in
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
                }
            }
            .padding(14)
        }
        .padding(.horizontal, 8)
        .frame(maxHeight: .infinity, alignment: .top)
        .background(Theme.surface)
    }
}

private struct SidebarRow: View {
    let title: String
    let symbol: String
    var badge = 0
    var attention = false
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: symbol)
                    .frame(width: 20)
                    .foregroundStyle(selected ? Theme.text : Theme.secondary)
                Text(title).foregroundStyle(Theme.text).lineLimit(1)
                Spacer()
                if badge > 0 {
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
            .background(selected ? Theme.raised : .clear, in: .rect(cornerRadius: 8))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
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
