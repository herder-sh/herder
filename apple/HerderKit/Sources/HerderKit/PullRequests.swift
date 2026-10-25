import Foundation
import Herder

/// A session's PRs and every descendant's, for its header's chip: this session's first, then
/// its children's by title, each followed by its own. A PR linked to two sessions in the tree
/// shows once, under the first.
struct PRRollup: Equatable {
    /// Each session's open, draft, merged and closed; newest first within each.
    let all: [PullRequest]

    /// `session` looks a session up, so a view that rolls one tree up reads only that tree's
    /// sessions; `children` holds each session's children.
    init(of key: SessionKey, session: (SessionKey) -> SessionModel?, children: [SessionKey: [SessionKey]]) {
        var seen = Set<String>()
        var all: [PullRequest] = []
        func visit(_ key: SessionKey, depth: Int) {
            guard let model = session(key) else { return }
            all += model.prs.filter { seen.insert($0.url).inserted }.sorted(by: PRRollup.order)
            guard depth < 8 else { return }
            let children = (children[key] ?? []).compactMap(session)
                .sorted { ($0.title ?? "").localizedStandardCompare($1.title ?? "") == .orderedAscending }
            for child in children { visit(child.key, depth: depth + 1) }
        }
        visit(key, depth: 0)
        self.all = all
    }

    static func order(_ a: PullRequest, _ b: PullRequest) -> Bool {
        a.state.order != b.state.order ? a.state.order < b.state.order : a.number > b.number
    }

    var open: Int { all.filter { $0.state.rank == 0 }.count }

    /// The chip's text: a single PR's number, else the count and how many are open.
    var chip: String {
        if all.count == 1 { return "#\(all[0].number)" }
        return open == 0 ? "\(all.count) PRs" : "\(all.count) PRs · \(open) open"
    }

    /// The chip where the header drops its buttons' labels: open of all, as "3/16".
    var shortChip: String {
        if all.count == 1 { return "#\(all[0].number)" }
        return open == 0 ? "\(all.count)" : "\(open)/\(all.count)"
    }

    /// The most urgent state among them, which tints the chip.
    var urgent: PrState? { all.map(\.state).min { $0.order < $1.order } }
}

extension PrState {
    /// Open, then draft, merged and closed: how a session's PRs sort.
    var order: Int {
        switch self {
        case .open: 0
        case .draft: 1
        case .merged: 2
        case .closed: 3
        }
    }

    var word: String {
        switch self {
        case .open: "Open"
        case .draft: "Draft"
        case .merged: "Merged"
        case .closed: "Closed"
        }
    }
}
