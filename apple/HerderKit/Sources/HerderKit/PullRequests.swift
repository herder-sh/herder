import Foundation
import Herder
import SwiftUI

/// A session's pull requests, under its project, for the Pull Requests pane.
struct PRGroup: Hashable, Identifiable {
    var id: String { project }
    let project: String
    let sessions: [(session: SessionSummary, prs: [PullRequest])]

    static func == (a: Self, b: Self) -> Bool {
        a.project == b.project && a.sessions.map(\.session) == b.sessions.map(\.session)
            && a.sessions.map(\.prs) == b.sessions.map(\.prs)
    }

    func hash(into hasher: inout Hasher) {
        hasher.combine(project)
        for entry in sessions { hasher.combine(entry.session); hasher.combine(entry.prs) }
    }
}

extension Lists {
    /// Every session's PRs, open and draft first, grouped by project in the projects' order, as
    /// the TUI's `P` view lists them; `openOnly` keeps open and draft PRs.
    func pullRequests(openOnly: Bool) -> [PRGroup] {
        projects.compactMap { project in
            let sessions = project.sessions.compactMap { session -> (session: SessionSummary, prs: [PullRequest])? in
                let prs = session.prs
                    .filter { !openOnly || $0.state.rank == 0 }
                    .sorted { $0.state.rank < $1.state.rank }
                return prs.isEmpty ? nil : (session, prs)
            }
            return sessions.isEmpty ? nil : PRGroup(project: project.name, sessions: sessions)
        }
    }
}

/// The PR number in what the user typed: `123`, `#123`, or a `…/pull/123` link, as the TUI
/// accepts it (crates/herder-tui/src/prs.rs).
func prNumber(in text: String) -> UInt64? {
    let text = text.trimmingCharacters(in: .whitespaces)
    if let range = text.range(of: "/pull/") {
        return UInt64(text[range.upperBound...].prefix { $0.isNumber })
    }
    return UInt64(text.hasPrefix("#") ? String(text.dropFirst()) : text)
}

extension Fleet {
    func linkPR(_ number: UInt64, to key: SessionKey) async {
        await send(.linkPr(sessionId: key.sessionId, number: number), about: key)
    }

    func unlinkPR(_ number: UInt64, from key: SessionKey) async {
        await send(.unlinkPr(sessionId: key.sessionId, number: number), about: key)
    }
}

/// One PR as a row: state, number and title, then CI, review and mergeability, and its branch.
/// Clicking opens it in the browser.
struct PRRow: View {
    let pr: PullRequest
    let fleet: Fleet
    let key: SessionKey
    @Environment(\.openURL) private var openURL
    @State private var hovering = false

    var body: some View {
        Button { open() } label: {
            HStack(spacing: 12) {
                Image(systemName: pr.state == .merged ? "arrow.triangle.merge" : "arrow.triangle.pull")
                    .foregroundStyle(pr.state.color)
                    .frame(width: 16)
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Text("#\(pr.number)").font(.subheadline.weight(.semibold).monospacedDigit())
                            .foregroundStyle(pr.state.color)
                        Text(pr.title).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text).lineLimit(1)
                    }
                    HStack(spacing: 10) {
                        Text(pr.state.word).foregroundStyle(pr.state.color)
                        ci
                        if pr.state.rank == 0 {
                            review
                            merge
                        }
                        if let branch = pr.headBranch {
                            Text(branch).font(Theme.monoSmall).foregroundStyle(Theme.tertiary).lineLimit(1)
                        }
                    }
                    .font(.caption)
                }
                Spacer()
                Image(systemName: "arrow.up.right").font(.caption).foregroundStyle(hovering ? Theme.secondary : Theme.tertiary)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(minHeight: 44)
            .background(hovering ? Theme.raised : .clear, in: .rect(cornerRadius: Theme.corner - 2))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help(pr.url)
        .contextMenu {
            Button("Open in Browser", systemImage: "safari") { open() }
            Button("Copy Link", systemImage: "link") { Clipboard.string = pr.url }
            Divider()
            Button("Unlink from Session", systemImage: "minus.circle") { Task { await fleet.unlinkPR(pr.number, from: key) } }
        }
    }

    private func open() {
        if let url = URL(string: pr.url) { openURL(url) }
    }

    @ViewBuilder private var ci: some View {
        let dim = pr.state.rank != 0
        switch pr.ci {
        case .passing: Label("CI", systemImage: "checkmark").foregroundStyle(dim ? Theme.tertiary : Theme.success)
        case .failing: Label("CI", systemImage: "xmark").foregroundStyle(dim ? Theme.tertiary : Theme.failure)
        case .pending: Label("CI", systemImage: "circle.dotted").foregroundStyle(dim ? Theme.tertiary : Theme.accent)
        case .none: Text("–").foregroundStyle(Theme.tertiary)
        }
    }

    @ViewBuilder private var review: some View {
        switch pr.review {
        case .approved: Label("Approved", systemImage: "checkmark").foregroundStyle(Theme.success)
        case .changesRequested: Label("Changes", systemImage: "xmark").foregroundStyle(Theme.failure)
        case .required: Label("Review", systemImage: "circle.dotted").foregroundStyle(Theme.accent)
        case .none: EmptyView()
        }
    }

    @ViewBuilder private var merge: some View {
        switch pr.mergeable {
        case .clean: Label("Merge", systemImage: "checkmark").foregroundStyle(Theme.secondary)
        case .conflicting: Label("Conflict", systemImage: "xmark").foregroundStyle(Theme.failure)
        case .unknown: Label("Merge", systemImage: "questionmark").foregroundStyle(Theme.tertiary)
        }
    }
}

extension PrState {
    var word: String {
        switch self {
        case .open: "Open"
        case .draft: "Draft"
        case .merged: "Merged"
        case .closed: "Closed"
        }
    }
}

/// How a session's PRs show from its header: a 520 point popover where there is room for one,
/// else a sheet, as a phone is narrower than the popover.
enum PRListPresentation: Equatable {
    case popover, sheet

    init(compact: Bool) {
        self = compact ? .sheet : .popover
    }
}

/// A session's PRs, at most four, with linking another.
struct PRStrip: View {
    let fleet: Fleet
    let key: SessionKey
    let prs: [PullRequest]
    let presentation: PRListPresentation
    @State private var expanded = false
    @State private var linking = false
    @State private var typed = ""

    var body: some View {
        Group {
            switch presentation {
            case .popover:
                VStack(alignment: .leading, spacing: 2) {
                    rows
                    HStack { actions }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 4)
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
                .frame(width: 520)
                .background(Theme.surface)
                .preferredColorScheme(.dark)
            case .sheet:
                SheetScaffold(title: "Pull Requests", subtitle: "Linked to this session.") {
                    VStack(alignment: .leading, spacing: 2) { rows }
                        .padding(.horizontal, -12)
                } footer: {
                    actions
                }
                .presentationDetents([.medium, .large])
            }
        }
        .alert("Link a pull request", isPresented: $linking) {
            TextField("123, #123 or a link", text: $typed)
            Button("Link") {
                if let number = prNumber(in: typed) { Task { await fleet.linkPR(number, to: key) } }
                typed = ""
            }
            Button("Cancel", role: .cancel) { typed = "" }
        } message: {
            Text("Its number, or its link.")
        }
    }

    private var sorted: [PullRequest] { prs.sorted { $0.state.rank < $1.state.rank } }

    private var rows: some View {
        ForEach(expanded ? sorted : Array(sorted.prefix(4)), id: \.number) { PRRow(pr: $0, fleet: fleet, key: key) }
    }

    private var actions: some View {
        Group {
            if sorted.count > 4 {
                Button(expanded ? "Show fewer" : "Show all \(sorted.count)") { expanded.toggle() }
            }
            Spacer()
            Button("Link PR…", systemImage: "link") { linking = true }
        }
        .buttonStyle(.plain)
        .font(.caption.weight(.medium))
        .foregroundStyle(Theme.secondary)
    }
}

/// Every session's PRs, by project and session; a session opens beside them, or is pushed.
struct PullRequestsView: View {
    let fleet: Fleet
    /// Where a tapped session opens on iPad and the Mac; `nil` pushes it.
    var selection: Binding<SessionKey?>?
    var query = ""
    @AppStorage("prsOpenOnly") private var openOnly = true

    var body: some View {
        let groups = fleet.lists.pullRequests(openOnly: openOnly).compactMap { group -> PRGroup? in
            let sessions = group.sessions.filter { $0.session.matches(query) }
            return sessions.isEmpty ? nil : PRGroup(project: group.project, sessions: sessions)
        }
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 18) {
                Picker("Show", selection: $openOnly) {
                    Text("Open").tag(true)
                    Text("All").tag(false)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 180)
                if groups.isEmpty {
                    Text(openOnly ? "No open pull requests." : "No pull requests linked to sessions yet.")
                        .foregroundStyle(Theme.tertiary)
                        .padding(.top, 30)
                }
                ForEach(groups) { group in
                    VStack(alignment: .leading, spacing: 8) {
                        SectionHeading(title: group.project)
                        ForEach(group.sessions, id: \.session.key) { entry in
                            VStack(alignment: .leading, spacing: 2) {
                                SessionButton(key: entry.session.key, open: selection.map { selection in { selection.wrappedValue = $0 } }) {
                                    HStack(spacing: 8) {
                                        StatusGlyph(state: entry.session.state, size: 7)
                                        Text(entry.session.title).font(.subheadline.weight(.semibold))
                                            .foregroundStyle(Theme.text).lineLimit(1)
                                        Spacer()
                                        Text(entry.session.machine).font(.caption).foregroundStyle(Theme.tertiary)
                                    }
                                    .padding(.horizontal, 12)
                                    .padding(.top, 10)
                                    .contentShape(.rect)
                                }
                                ForEach(entry.prs, id: \.number) { PRRow(pr: $0, fleet: fleet, key: entry.session.key) }
                            }
                            .padding(.bottom, 6)
                            .background(selection?.wrappedValue == entry.session.key ? Theme.raised.opacity(0.6) : Theme.surface,
                                        in: .rect(cornerRadius: Theme.corner))
                        }
                    }
                }
            }
            .padding(16)
        }
        .background(Theme.background)
        .refreshable { fleet.wake() }
        .navigationTitle("Pull Requests")
        .navigationDestination(for: SessionKey.self) { SessionView(fleet: fleet, key: $0) }
    }
}
