import Herder
import SwiftUI

/// A task tree: its top session, and under it the rest of the tree.
struct TaskTree: Hashable, Identifiable {
    var id: SessionKey { lead.key }
    let lead: SessionSummary
    /// The rest of the tree, in tree order.
    let children: [SessionSummary]

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

extension Lists {
    /// Every task tree with a session that is neither archived nor moved, newest first. A child
    /// never stands on its own: it is listed under the top of its tree.
    var board: [TaskTree] {
        // A project's live sessions are whole task trees.
        projects.flatMap { TaskTree.trees($0.live) }
            .filter { tree in ([tree.lead] + tree.children).contains { ![.archived, .moved].contains($0.state) } }
            // Session ids are ULIDs, which sort by creation time.
            .sorted { ($0.lead.key.sessionId, $0.lead.key.hostId) > ($1.lead.key.sessionId, $1.lead.key.hostId) }
    }

    /// The Board's task trees a search finds.
    func board(matching query: String) -> [TaskTree] {
        board.filter { $0.matches(query) }
    }
}

/// The app's home: what is waiting on you, then every task tree, newest first, each with its
/// agents folded under its top; a session opens beside it, or is pushed.
struct BoardView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    /// Where a tapped session opens on iPad and the Mac; `nil` pushes it.
    var selection: Binding<SessionKey?>?
    var query = ""
    /// Leads to Pull Requests, where it has no section of its own.
    var showsPullRequests = false

    var body: some View {
        let lists = fleet.lists
        let trees = lists.board(matching: query)
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 22) {
                #if os(iOS)
                // On the phone; iPad's shell says it under the Board's title.
                if selection == nil { ConnectionLine(machines: lists.machines) }
                #endif
                if lists.machines.isEmpty {
                    EmptyFleet { sheet = .pair }
                }
                if !lists.requests.isEmpty && query.isEmpty {
                    VStack(alignment: .leading, spacing: 10) {
                        SectionHeading(title: "Requests", count: lists.requests.count, tint: Theme.accent)
                        ForEach(lists.requests) { RequestCard(request: $0, fleet: fleet, selection: selection) }
                    }
                }
                if showsPullRequests { pullRequestsLink }
                if trees.isEmpty && !lists.machines.isEmpty {
                    Text(query.isEmpty ? "No sessions yet." : "No sessions match.")
                        .foregroundStyle(Theme.tertiary)
                        .padding(.top, 30)
                }
                TaskTreeList(trees: trees, fleet: fleet, selection: selection)
            }
            .frame(maxWidth: 760)
            .frame(maxWidth: .infinity)
            .padding(16)
        }
        .background(Theme.background)
        .refreshable { fleet.wake() }
        .navigationTitle("Board")
    }

    private var pullRequestsLink: some View {
        NavigationLink { PullRequestsView(fleet: fleet) } label: {
            Card(padding: 12) {
                HStack(spacing: 10) {
                    Image(systemName: "arrow.triangle.pull").foregroundStyle(Theme.secondary)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Pull Requests").font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                        let open = fleet.lists.pullRequests(openOnly: true).flatMap(\.sessions).map(\.prs.count)
                            .reduce(0, +)
                        Text(open == 1 ? "1 open" : "\(open) open").font(.caption).foregroundStyle(Theme.secondary)
                    }
                    Spacer()
                    Image(systemName: "chevron.right").font(.caption.weight(.bold)).foregroundStyle(Theme.tertiary)
                }
            }
        }
        .buttonStyle(.plain)
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
