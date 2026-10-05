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

/// The sessions in one of the Board's columns.
struct BoardColumn: Hashable, Identifiable {
    var id: WorkState { state }
    let state: WorkState
    let sessions: [SessionSummary]
}

extension Lists {
    /// Every session that is not archived or moved, under where its work stands, what needs
    /// the user first; newest first within each.
    var board: [BoardColumn] {
        let sessions = projects.flatMap(\.live)
            .filter { ![.archived, .moved].contains($0.state) }
            .map { session -> SessionSummary in
                var flat = session
                flat.depth = 0
                return flat
            }
        let columns = Dictionary(grouping: sessions) { WorkState(state: $0.state, prs: $0.prs) }
        return WorkState.allCases.compactMap { state in
            guard let members = columns[state] else { return nil }
            // Session ids are ULIDs, which sort by creation time.
            let newest = members.sorted { ($0.key.sessionId, $0.key.hostId) > ($1.key.sessionId, $1.key.hostId) }
            return BoardColumn(state: state, sessions: newest)
        }
    }
}

/// Every session by where its work stands, so "did CI pass?" needs no asking; a session opens
/// beside it, or is pushed.
struct BoardView: View {
    let fleet: Fleet
    /// Where a tapped session opens on iPad and the Mac; `nil` pushes it.
    var selection: Binding<SessionKey?>?
    var query = ""
    /// Leads to Pull Requests, where it has no section of its own.
    var showsPullRequests = false

    var body: some View {
        let columns = fleet.lists.board.compactMap { column -> BoardColumn? in
            let sessions = column.sessions.filter { $0.matches(query) }
            return sessions.isEmpty ? nil : BoardColumn(state: column.state, sessions: sessions)
        }
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 22) {
                if showsPullRequests { pullRequestsLink }
                if columns.isEmpty {
                    Text(query.isEmpty ? "No sessions yet." : "No sessions match.")
                        .foregroundStyle(Theme.tertiary)
                        .padding(.top, 30)
                }
                ForEach(columns) { column in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack(spacing: 6) {
                            Image(systemName: column.state.symbol)
                            SectionHeading(title: column.state.label, count: column.sessions.count,
                                           tint: column.state.tint)
                        }
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(column.state.tint)
                        .accessibilityElement(children: .combine)
                        SessionGroup(title: nil, sessions: column.sessions, fleet: fleet, selection: selection)
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
        .navigationDestination(for: SessionKey.self) { SessionView(fleet: fleet, key: $0) }
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
