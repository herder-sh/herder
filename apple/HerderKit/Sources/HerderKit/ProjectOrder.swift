import Foundation

/// How the sidebar orders the projects, saved on this device.
enum ProjectSort: String, CaseIterable {
    /// As the user dragged them; the default.
    case manual
    case recent
    case needsYou
    case name

    var label: String {
        switch self {
        case .manual: "Manual"
        case .recent: "Recent activity"
        case .needsYou: "Needs you first"
        case .name: "Name"
        }
    }
}

/// The sidebar's projects in the order the user chose, the quiet ones folded away. The sessions
/// no project holds yet stay last whatever the order.
struct ProjectOrder {
    /// Where this device keeps the sort and the manual order, the ids one per line.
    static let sortKey = "projectSort"
    static let manualKey = "projectOrder"

    let sort: ProjectSort
    /// The projects' ids as the user dragged them; a project not among them follows them, by name.
    let manual: [String]

    /// `projects` in this order.
    func sorted(_ projects: [ProjectGroup]) -> [ProjectGroup] {
        let place = Dictionary(manual.enumerated().map { ($1, $0) }) { first, _ in first }
        return projects.sorted { a, b in
            if (a.projectId == nil) != (b.projectId == nil) { return b.projectId == nil }
            let byName = (a.name.lowercased(), a.id) < (b.name.lowercased(), b.id)
            switch sort {
            case .manual:
                let (x, y) = (place[a.id] ?? .max, place[b.id] ?? .max)
                return x != y ? x < y : byName
            case .recent:
                return a.recency < b.recency
            case .needsYou:
                if a.needsYou != b.needsYou { return a.needsYou > b.needsYou }
                if a.working != b.working { return a.working > b.working }
                return a.recency < b.recency
            case .name:
                return byName
            }
        }
    }

    /// `projects` in this order, split into those shown and the quiet ones folded under "N
    /// more". A quiet project that waits for the user, or the one open, stays shown.
    func folded(_ projects: [ProjectGroup], open: SidebarItem) -> (shown: [ProjectGroup], quiet: [ProjectGroup]) {
        let sorted = sorted(projects)
        func folds(_ project: ProjectGroup) -> Bool {
            project.quiet && project.needsYou == 0 && project.working == 0 && open != .project(project.id)
        }
        return (sorted.filter { !folds($0) }, sorted.filter(folds))
    }

    init(sort: ProjectSort, manual: [String]) {
        self.sort = sort
        self.manual = manual
    }

    /// The order as this device keeps it.
    init(sort: ProjectSort, stored: String) {
        self.init(sort: sort, manual: stored.split(separator: "\n").map(String.init))
    }

    /// The manual order after dragging `moved` onto `target`, from `shown`, the order the
    /// sidebar shows: it takes the target's place, as a list's drag does.
    static func move(_ moved: String, onto target: String, in shown: [String]) -> [String] {
        guard moved != target, let from = shown.firstIndex(of: moved), let to = shown.firstIndex(of: target) else {
            return shown
        }
        var ids = shown
        ids.remove(at: from)
        ids.insert(moved, at: to)
        return ids
    }
}
