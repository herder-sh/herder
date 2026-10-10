import Herder
import SwiftUI

/// Where a session's work stands, from its state and its linked PRs: the Board's columns, in
/// the order it lists them, what needs the user first.
enum WorkState: Int, CaseIterable, Comparable, Identifiable {
    /// Waiting on an approval or a question, or stopped on an error.
    case needsYou
    case ciFailed
    case changesRequested
    case conflicting
    /// An open PR whose checks passed (or that has none) and that merges cleanly.
    case readyToMerge
    /// Stopped with nothing to merge: no PR, only closed ones, or a draft.
    case idle
    /// Running, or waiting for capacity.
    case working
    /// Stopped with an open PR whose checks run, or whose mergeability GitHub still computes.
    case waitingOnCI
    /// Stopped with its PRs merged and none open.
    case merged

    var id: Self { self }

    static func < (a: Self, b: Self) -> Bool { a.rawValue < b.rawValue }

    init(state: SessionState, prs: [PullRequest]) {
        switch state {
        case .needsYou, .error:
            self = .needsYou
        case .running, .waiting:
            self = .working
        case .idle, .done, .archived, .moved:
            let open = prs.filter { $0.state.rank == 0 }.map(WorkState.init(pr:))
            if let urgent = open.min() {
                self = urgent
            } else {
                self = prs.contains { $0.state == .merged } ? .merged : .idle
            }
        }
    }

    /// One open or draft PR's, its most urgent problem first.
    init(pr: PullRequest) {
        if pr.ci == .failing {
            self = .ciFailed
        } else if pr.review == .changesRequested {
            self = .changesRequested
        } else if pr.mergeable == .conflicting {
            self = .conflicting
        } else if pr.ci == .pending || pr.mergeable == .unknown {
            self = .waitingOnCI
        } else {
            self = pr.state == .draft ? .idle : .readyToMerge
        }
    }

    var label: String {
        switch self {
        case .needsYou: "Needs you"
        case .ciFailed: "CI failed"
        case .changesRequested: "Changes requested"
        case .conflicting: "Conflicting"
        case .readyToMerge: "Ready to merge"
        case .idle: "Idle"
        case .working: "Working"
        case .waitingOnCI: "Waiting on CI"
        case .merged: "Merged"
        }
    }

    var symbol: String {
        switch self {
        case .needsYou: "exclamationmark.circle.fill"
        case .ciFailed: "xmark.circle.fill"
        case .changesRequested: "text.bubble"
        case .conflicting: "arrow.triangle.branch"
        case .readyToMerge: "checkmark.circle.fill"
        case .idle: "circle"
        case .working: "circle.dotted"
        case .waitingOnCI: "clock"
        case .merged: "arrow.triangle.merge"
        }
    }

    var tint: Color {
        switch self {
        case .needsYou: Theme.accent
        case .ciFailed, .changesRequested, .conflicting: Theme.failure
        case .readyToMerge: Theme.success
        case .idle: Theme.secondary
        case .working: Theme.running
        case .waitingOnCI: Theme.waiting
        case .merged: Theme.merged
        }
    }
}

/// A task tree on the Board: its top session, and under it the rest of the tree.
struct BoardTree: Hashable, Identifiable {
    var id: SessionKey { lead.key }
    let lead: SessionSummary
    /// The rest of the tree, in tree order.
    let children: [SessionSummary]

    /// What the Board lists under the top: the live sessions in tree order, then the archived.
    var listed: [SessionSummary] { live + archived }
    var live: [SessionSummary] { children.filter { $0.state != .archived } }
    var archived: [SessionSummary] { children.filter { $0.state == .archived } }

    /// Whether a search finds the tree: its top or any session under it matches.
    func matches(_ query: String) -> Bool {
        ([lead] + children).contains { $0.matches(query) }
    }
}

/// The task trees in one of the Board's columns.
struct BoardColumn: Hashable, Identifiable {
    var id: WorkState { state }
    let state: WorkState
    let trees: [BoardTree]
}

extension Lists {
    /// Every task tree with a session that is neither archived nor moved, under where its most
    /// urgent such session's work stands, what needs the user first; newest first within each.
    /// A child never stands on its own: it is listed under the top of its tree.
    var board: [BoardColumn] {
        // A project's live sessions are whole task trees: each top (depth 0), then the rest.
        var trees: [[SessionSummary]] = []
        for session in projects.flatMap(\.live) {
            if session.depth == 0 || trees.isEmpty { trees.append([session]) } else { trees[trees.count - 1].append(session) }
        }
        let placed = trees.compactMap { members -> (BoardTree, WorkState)? in
            let active = members.filter { ![.archived, .moved].contains($0.state) }
            guard let state = active.map({ WorkState(state: $0.state, prs: $0.prs) }).min() else { return nil }
            return (BoardTree(lead: members[0], children: Array(members.dropFirst())), state)
        }
        let columns = Dictionary(grouping: placed, by: \.1)
        return WorkState.allCases.compactMap { state in
            guard let members = columns[state] else { return nil }
            // Session ids are ULIDs, which sort by creation time.
            let newest = members.map(\.0).sorted {
                ($0.lead.key.sessionId, $0.lead.key.hostId) > ($1.lead.key.sessionId, $1.lead.key.hostId)
            }
            return BoardColumn(state: state, trees: newest)
        }
    }

    /// The Board's columns with just the task trees a search finds, and without the columns it
    /// leaves empty.
    func board(matching query: String) -> [BoardColumn] {
        board.compactMap { column in
            let trees = column.trees.filter { $0.matches(query) }
            return trees.isEmpty ? nil : BoardColumn(state: column.state, trees: trees)
        }
    }
}

/// The app's home: what is waiting on you, then every task tree by where its work stands, so
/// "did CI pass?" needs no asking; a session opens beside it, or is pushed.
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
        let columns = lists.board(matching: query)
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
                        SectionHeading(title: "Requests", count: lists.requests.count, tint: Theme.accent)
                        ForEach(lists.requests) { RequestCard(request: $0, fleet: fleet, selection: selection) }
                    }
                }
                if showsPullRequests { pullRequestsLink }
                if columns.isEmpty && !lists.machines.isEmpty {
                    Text(query.isEmpty ? "No sessions yet." : "No sessions match.")
                        .foregroundStyle(Theme.tertiary)
                        .padding(.top, 30)
                }
                ForEach(columns) { column in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack(spacing: 6) {
                            Image(systemName: column.state.symbol)
                            SectionHeading(title: column.state.label, count: column.trees.count,
                                           tint: column.state.tint)
                        }
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(column.state.tint)
                        .accessibilityElement(children: .combine)
                        VStack(spacing: 0) {
                            ForEach(Array(column.trees.enumerated()), id: \.element.id) { index, tree in
                                if index > 0 { Divider().overlay(Theme.stroke).padding(.leading, 42) }
                                BoardTreeRows(tree: tree, fleet: fleet, selection: selection)
                            }
                        }
                        .padding(4)
                        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
                    }
                }
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

/// A task tree's rows on the Board: its top session's row and its provider's own agents, then
/// its herder children as sub-agent rows, the archived ones last, folded when there are several.
private struct BoardTreeRows: View {
    let tree: BoardTree
    let fleet: Fleet
    let selection: Binding<SessionKey?>?
    @State private var showsArchived = false

    var body: some View {
        SessionLink(session: tree.lead, fleet: fleet, selection: selection)
        ForEach(tree.lead.agents) { agent in
            NativeAgentRow(agent: agent, fleet: fleet, key: tree.lead.key)
        }
        ForEach(tree.live) { child in
            BoardChildRow(session: child, fleet: fleet, selection: selection)
        }
        let archived = tree.archived
        if archived.count == 1 || showsArchived {
            ForEach(archived) { child in
                BoardChildRow(session: child, fleet: fleet, selection: selection)
            }
        } else if archived.count > 1 {
            Button { showsArchived = true } label: {
                HStack(spacing: 8) {
                    TreeLine().frame(width: 14)
                    Image(systemName: "archivebox").font(.caption).frame(width: 20, height: 20)
                    Text("\(archived.count) archived").font(.subheadline)
                    Spacer(minLength: 6)
                    Image(systemName: "chevron.right").font(.caption2.weight(.bold))
                }
                .foregroundStyle(Theme.tertiary)
                .padding(.vertical, 5)
                .padding(.horizontal, 8)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Show \(archived.count) archived sub-sessions")
        }
    }
}

/// A herder child under the top of its task tree on the Board, drawn as a provider's agent is
/// (`NativeAgentRow`), badged with where its own work stands. It opens the child, into
/// `selection` when given, else by pushing it, as `SessionLink` does.
private struct BoardChildRow: View {
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
        let work = WorkState(state: session.state, prs: session.prs)
        let badge = archived ? "Archived" : work.label
        HStack(spacing: 8) {
            TreeLine().frame(width: CGFloat(max(session.depth, 1)) * 14)
            ProviderMark(provider: fleet.sessions[session.key]?.provider ?? "claude", size: 11)
                .frame(width: 20, height: 20)
                .background(Theme.raised, in: Circle())
                .overlay(Circle().strokeBorder(Theme.stroke))
            Text(session.title).font(.subheadline).foregroundStyle(Theme.text).lineLimit(1)
            Spacer(minLength: 6)
            if !archived && work == .working { ProgressView().controlSize(.mini) }
            Text(badge).font(.caption.weight(.semibold)).foregroundStyle(archived ? Theme.tertiary : work.tint)
                .fixedSize()
        }
        .padding(.vertical, 5)
        .padding(.horizontal, 8)
        .background(selected || hovering ? Theme.raised : .clear, in: .rect(cornerRadius: 7))
        .contentShape(.rect)
        .opacity(archived ? 0.75 : 1)
        .onHover { hovering = $0 }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Open sub-session: \(session.title), \(badge)")
    }
}
