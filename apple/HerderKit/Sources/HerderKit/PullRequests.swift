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

/// A session's PRs and every descendant's, for its header: one group per session that has
/// any, this session first, then its children by title, each followed by its own. A PR linked
/// to two sessions in the tree shows once, under the first.
struct PRRollup: Equatable {
    struct Group: Equatable, Identifiable {
        var id: SessionKey { key }
        let key: SessionKey
        let title: String
        /// 0 for this session, 1 for a child, 2 for a grandchild.
        let depth: Int
        /// Open, draft, merged, closed; newest first within each.
        let prs: [PullRequest]

        var live: [PullRequest] { prs.filter { $0.state.rank == 0 } }
        var finished: [PullRequest] { prs.filter { $0.state.rank != 0 } }
    }

    let groups: [Group]

    init(of key: SessionKey, sessions: [SessionKey: SessionModel]) {
        var seen = Set<String>()
        var groups: [Group] = []
        func visit(_ key: SessionKey, depth: Int) {
            guard let model = sessions[key] else { return }
            let prs = model.prs.filter { seen.insert($0.url).inserted }.sorted(by: PRRollup.order)
            if !prs.isEmpty {
                groups.append(Group(key: key, title: model.title ?? "Session …\(key.sessionId.suffix(6))",
                                    depth: depth, prs: prs))
            }
            guard depth < 8 else { return }
            let children = sessions.values
                .filter { $0.key.hostId == key.hostId && $0.parent == key.sessionId }
                .sorted { ($0.title ?? "").localizedStandardCompare($1.title ?? "") == .orderedAscending }
            for child in children { visit(child.key, depth: depth + 1) }
        }
        visit(key, depth: 0)
        self.groups = groups
    }

    static func order(_ a: PullRequest, _ b: PullRequest) -> Bool {
        a.state.order != b.state.order ? a.state.order < b.state.order : a.number > b.number
    }

    var all: [PullRequest] { groups.flatMap(\.prs) }
    var open: Int { all.filter { $0.state.rank == 0 }.count }

    /// The header button's text: a single PR's number, else the count and how many are open.
    var chip: String {
        let all = all
        if all.count == 1 { return "#\(all[0].number)" }
        return open == 0 ? "\(all.count) PRs" : "\(all.count) PRs · \(open) open"
    }

    /// The chip where the header drops its buttons' labels: open of all, as "3/16".
    var shortChip: String {
        let all = all
        if all.count == 1 { return "#\(all[0].number)" }
        return open == 0 ? "\(all.count)" : "\(open)/\(all.count)"
    }

    /// The most urgent state among them, which tints the header button.
    var urgent: PrState? { all.map(\.state).min { $0.order < $1.order } }

    /// A search field shows once there are more PRs than this.
    static let searchFrom = 10

    /// The groups as the list shows them: open and draft only when `openOnly`, and those whose
    /// number, title or branch match `query`.
    func shown(openOnly: Bool, query: String) -> [Group] {
        let query = query.trimmingCharacters(in: .whitespaces).lowercased()
        let number = query.hasPrefix("#") ? String(query.dropFirst()) : query
        return groups.compactMap { group in
            let prs = group.prs.filter { pr in
                (!openOnly || pr.state.rank == 0)
                    && (query.isEmpty || String(pr.number).hasPrefix(number) || pr.title.lowercased().contains(query)
                        || pr.headBranch?.lowercased().contains(query) == true)
            }
            return prs.isEmpty ? nil : Group(key: group.key, title: group.title, depth: group.depth, prs: prs)
        }
    }

    /// What stands for a group's merged and closed PRs while they are folded away.
    static func folded(_ prs: [PullRequest]) -> String {
        let merged = prs.filter { $0.state == .merged }.count
        let closed = prs.count - merged
        return switch (merged, closed) {
        case (_, 0): "\(merged) merged"
        case (0, _): "\(closed) closed"
        default: "\(merged) merged · \(closed) closed"
        }
    }
}

extension PrState {
    /// Open, then draft, merged and closed: how a session's PR list sorts.
    var order: Int {
        switch self {
        case .open: 0
        case .draft: 1
        case .merged: 2
        case .closed: 3
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

/// A session's PRs and its descendants' from its header, grouped by session: Open or All,
/// each group's merged and closed folded away, a search once there are many, in a capped
/// scroll built lazily, so it holds a hundred PRs.
struct PRStrip: View {
    let fleet: Fleet
    let key: SessionKey
    let rollup: PRRollup
    let presentation: PRListPresentation
    /// Opens a descendant's session.
    let open: (SessionKey) -> Void
    /// `nil` until the user picks: Open while any is open, else All.
    @State private var openOnly: Bool?
    @State private var query = ""
    /// Groups whose merged and closed PRs are unfolded.
    @State private var unfolded: Set<SessionKey> = []
    @State private var linking = false
    @State private var typed = ""

    var body: some View {
        Group {
            switch presentation {
            case .popover:
                VStack(alignment: .leading, spacing: 0) {
                    if hasControls {
                        controls.padding(.horizontal, 12).padding(.vertical, 10)
                        Rectangle().fill(Theme.stroke).frame(height: 1)
                    }
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 10) { groups }
                            .padding(8)
                    }
                    .frame(maxHeight: 460)
                    .fixedSize(horizontal: false, vertical: true)
                    Rectangle().fill(Theme.stroke).frame(height: 1)
                    HStack { actions }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                }
                .frame(width: 520)
                .background(Theme.surface)
                .preferredColorScheme(.dark)
            case .sheet:
                SheetScaffold(title: "Pull Requests", subtitle: rollup.groups.count > 1
                              ? "This session's and its agents'." : "Linked to this session.") {
                    if hasControls { controls }
                    LazyVStack(alignment: .leading, spacing: 10) { groups }
                        .padding(.horizontal, -10)
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

    private var actions: some View {
        Group {
            Spacer()
            Button("Link PR…", systemImage: "link") { linking = true }
        }
        .buttonStyle(.plain)
        .font(.caption.weight(.medium))
        .foregroundStyle(Theme.secondary)
    }

    private var filter: Binding<Bool> {
        Binding { openOnly ?? (rollup.open > 0) } set: { openOnly = $0 }
    }

    /// Open / All only when they differ; the search once there are many.
    private var filters: Bool { rollup.open > 0 && rollup.open < rollup.all.count }
    private var searches: Bool { rollup.all.count > PRRollup.searchFrom }
    private var hasControls: Bool { filters || searches }

    private var controls: some View {
        HStack(spacing: 10) {
            if filters {
                Picker("Show", selection: filter) {
                    Text("Open \(rollup.open)").tag(true)
                    Text("All \(rollup.all.count)").tag(false)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .fixedSize()
            }
            if searches {
                HStack(spacing: 6) {
                    Image(systemName: "magnifyingglass").foregroundStyle(Theme.tertiary)
                    TextField("Number, title or branch", text: $query)
                        .textFieldStyle(.plain)
                        .foregroundStyle(Theme.text)
                    if !query.isEmpty {
                        Button { query = "" } label: {
                            Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.tertiary)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Clear search")
                    }
                }
                .font(.subheadline)
                .padding(.horizontal, 10)
                .frame(height: 30)
                .background(Theme.background, in: .rect(cornerRadius: Theme.corner - 2))
            } else {
                Spacer()
            }
        }
    }

    @ViewBuilder private var groups: some View {
        let shown = rollup.shown(openOnly: filter.wrappedValue, query: query)
        if shown.isEmpty {
            Text(!query.isEmpty ? "No PRs match." : "No open PRs. Show All for the merged and closed.")
                .font(.subheadline).foregroundStyle(Theme.tertiary)
                .padding(12)
        }
        ForEach(shown) { group in
            VStack(alignment: .leading, spacing: 0) {
                // A session alone needs no heading: the list is its own.
                if rollup.groups.count > 1 { heading(group) }
                ForEach(group.live, id: \.url) { PRLine(pr: $0, fleet: fleet, key: group.key) }
                let finished = group.finished
                if !finished.isEmpty {
                    if unfolded.contains(group.key) || !query.isEmpty {
                        ForEach(finished, id: \.url) { PRLine(pr: $0, fleet: fleet, key: group.key) }
                    } else {
                        Button { unfolded.insert(group.key) } label: {
                            HStack(spacing: 6) {
                                Image(systemName: "chevron.right").font(.caption2.weight(.bold))
                                Text(PRRollup.folded(finished))
                            }
                            .font(.caption.weight(.medium))
                            .foregroundStyle(Theme.tertiary)
                            .padding(.horizontal, 10)
                            .frame(maxWidth: .infinity, minHeight: 28, alignment: .leading)
                            .hitTarget()
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
            .padding(.leading, CGFloat(min(group.depth, 4)) * 14)
        }
    }

    /// A group's session: this one as it is, a descendant as a link to it.
    @ViewBuilder private func heading(_ group: PRRollup.Group) -> some View {
        let label = HStack(spacing: 6) {
            if group.depth > 0 {
                Image(systemName: "arrow.turn.down.right").foregroundStyle(Theme.child)
            }
            StatusGlyph(state: fleet.sessions[group.key]?.state ?? .idle, size: 6)
            Text(group.title).foregroundStyle(group.depth == 0 ? Theme.secondary : Theme.text).lineLimit(1)
            Text("\(group.prs.count)").foregroundStyle(Theme.tertiary)
            Spacer(minLength: 0)
            if group.depth > 0 {
                Image(systemName: "chevron.right").font(.caption2.weight(.bold)).foregroundStyle(Theme.tertiary)
            }
        }
        .font(.caption.weight(.semibold))
        .padding(.horizontal, 10)
        .frame(minHeight: 26)
        .hitTarget()
        if group.depth == 0 {
            label
        } else {
            Button { open(group.key) } label: { label }
                .buttonStyle(.plain)
                .help("Open \(group.title)")
                .accessibilityLabel("Open session \(group.title)")
        }
    }
}

/// One PR on one line: state, number, title, state word and CI. Clicking opens it in the browser.
struct PRLine: View {
    let pr: PullRequest
    let fleet: Fleet
    let key: SessionKey
    @Environment(\.openURL) private var openURL
    @State private var hovering = false

    var body: some View {
        Button { open() } label: {
            HStack(spacing: 8) {
                Image(systemName: pr.state == .merged ? "arrow.triangle.merge" : "arrow.triangle.pull")
                    .font(.caption)
                    .foregroundStyle(pr.state.color)
                    .frame(width: 14)
                Text("#\(pr.number)").font(.subheadline.weight(.semibold).monospacedDigit())
                    .foregroundStyle(pr.state.color)
                Text(pr.title).font(.subheadline).foregroundStyle(pr.state.rank == 0 ? Theme.text : Theme.secondary)
                    .lineLimit(1).truncationMode(.tail)
                Spacer(minLength: 6)
                Text(pr.state.word).font(.caption).foregroundStyle(pr.state.color)
                ci.font(.caption.weight(.bold)).frame(width: 14)
            }
            .padding(.horizontal, 10)
            .frame(minHeight: 32)
            .background(hovering ? Theme.raised : .clear, in: .rect(cornerRadius: Theme.corner - 2))
            .hitTarget()
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help(pr.headBranch.map { "\($0) · \(pr.url)" } ?? pr.url)
        .accessibilityLabel("Pull request \(pr.number), \(pr.title), \(pr.state.word)")
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
        case .passing: Image(systemName: "checkmark").foregroundStyle(dim ? Theme.tertiary : Theme.success)
        case .failing: Image(systemName: "xmark").foregroundStyle(dim ? Theme.tertiary : Theme.failure)
        case .pending: Image(systemName: "circle.dotted").foregroundStyle(dim ? Theme.tertiary : Theme.accent)
        case .none: Text("–").foregroundStyle(Theme.tertiary)
        }
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
