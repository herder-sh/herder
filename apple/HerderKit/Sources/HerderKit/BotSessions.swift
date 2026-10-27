import Herder
import SwiftUI

extension SessionOrigin {
    /// Where its person wrote to the bot: "DM", or the channel, "#general" on Slack.
    var placeLabel: String {
        switch place {
        case .direct: "DM"
        case .channel(let name): kind == .slack ? "#\(name)" : name
        case .unknown: ""
        }
    }

    /// What a bot's task row says before what the task is doing: who wrote, and where.
    var line: String {
        [by, placeLabel].filter { !$0.isEmpty }.joined(separator: " · ")
    }
}

extension Fleet {
    /// A bot's name as its machine configures it, once this device has listed its bots; else
    /// its id.
    func botName(_ group: BotGroup) -> String {
        bots.lists[group.hostId]?.bot(group.botId)?.name ?? group.botId
    }
}

extension Lists {
    /// The bots `filter` names, or every bot for `nil`.
    func bots(_ filter: String?) -> [BotGroup] {
        bots.filter { filter == nil || $0.id == filter }
    }

    /// How many of the bots' sessions wait for the user: Bots' count is that, not activity.
    var botsWaiting: Int { bots.map(\.waiting).reduce(0, +) }
}

/// The bots' conversations a search finds, a section a bot, the latest first: each task's title,
/// who wrote and where, and where it stands. The archived ones last, folded away until asked
/// for. Archiving the open task opens the next.
struct BotSessions: View {
    let fleet: Fleet
    let groups: [BotGroup]
    let query: String
    /// Where a tapped session opens beside the list; `nil` pushes it.
    var selection: Binding<SessionKey?>?
    @State private var showsArchived = false
    @Environment(\.undoManager) private var undoManager

    var body: some View {
        let sections = groups.map { group in
            (group: group, trees: Self.latest(TaskTree.trees(group.live).filter { $0.matches(query) }))
        }
        .filter { !$0.trees.isEmpty }
        let archived = Self.latest(groups.flatMap { TaskTree.trees($0.archived) }.filter { $0.matches(query) })
        let rows = sections.flatMap(\.trees) + (showsArchived ? archived : [])
        List {
            if sections.isEmpty && archived.isEmpty {
                Text(query.isEmpty ? "No bot conversations yet." : "No sessions match.")
                    .foregroundStyle(Theme.tertiary)
                    .padding(.vertical, 24)
                    .taskListRow()
            }
            ForEach(sections, id: \.group.id) { section in
                TaskSectionHeading(title: fleet.botName(section.group), count: section.trees.count)
                ForEach(section.trees, content: row)
            }
            if !archived.isEmpty {
                archivedLine(archived.count)
                if showsArchived {
                    ForEach(archived, content: row)
                }
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .environment(\.defaultMinListRowHeight, 0)
        #if os(macOS)
        .frame(maxWidth: 760)
        .frame(maxWidth: .infinity, alignment: .leading)
        #endif
        .modifier(TaskListNavigation(fleet: fleet, rows: rows, selection: selection) {
            $0.toggleArchived(fleet: fleet, undoManager: undoManager)
        })
    }

    private func row(_ tree: TaskTree) -> some View {
        TaskListRow(tree: tree, fleet: fleet, selection: selection, context: tree.lead.origin?.line ?? "") {
            tree.toggleArchived(fleet: fleet, undoManager: undoManager)
        }
    }

    /// "Archived · N", which shows or hides the archived tasks.
    private func archivedLine(_ count: Int) -> some View {
        Button {
            withAnimation(.snappy) { showsArchived.toggle() }
        } label: {
            HStack(spacing: 6) {
                Text("Archived · \(count)")
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.bold))
                    .rotationEffect(.degrees(showsArchived ? 90 : 0))
                Spacer()
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .font(.caption)
        .foregroundStyle(Theme.tertiary)
        .padding(.vertical, 10)
        .accessibilityLabel(showsArchived ? "Hide Archived" : "Show Archived")
        .taskListRow()
    }

    /// `trees`, the one that last did something first.
    nonisolated static func latest(_ trees: [TaskTree]) -> [TaskTree] {
        trees.sorted { ($0.lastActive ?? .distantPast) > ($1.lastActive ?? .distantPast) }
    }

    /// The Bots header's status: how many of the bots' tasks wait for you and work, then how
    /// many there are.
    nonisolated static func status(_ groups: [BotGroup]) -> String {
        ProjectSessions.status(live: groups.flatMap(\.live), archived: groups.flatMap(\.archived))
    }
}
