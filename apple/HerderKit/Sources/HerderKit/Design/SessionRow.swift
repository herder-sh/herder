import Herder
import SwiftUI

/// A session in a list: state, title, what it's doing, and where it runs.
struct SessionRow: View {
    let session: SessionSummary
    var showsProject = true
    /// The project's icon image, when known.
    var projectIcon: ProjectIconImage?

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            if session.depth > 0 {
                TreeLine().frame(width: CGFloat(session.depth) * 14)
            }
            StatusGlyph(state: session.state)
                .padding(.top, 1)
            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(session.title)
                        .font(.body.weight(.semibold))
                        .foregroundStyle(Theme.text)
                        .lineLimit(1)
                    if session.children > 0 {
                        Label("\(session.children)", systemImage: "point.3.connected.trianglepath.dotted")
                            .labelStyle(.titleAndIcon)
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(Theme.secondary)
                            .help(session.children == 1 ? "1 child session" : "\(session.children) child sessions")
                    }
                    if session.childrenNeedYou > 0 {
                        Text("\(session.childrenNeedYou) need you")
                            .font(.caption.weight(.bold))
                            .foregroundStyle(Theme.accent)
                    }
                    Spacer(minLength: 4)
                    Text(session.age)
                        .font(.caption)
                        .foregroundStyle(Theme.tertiary)
                }
                Text(session.activity)
                    .font(.subheadline)
                    .foregroundStyle(session.state == .needsYou ? Theme.accent : Theme.secondary)
                    .lineLimit(1)
                HStack(spacing: 6) {
                    if showsProject && !session.project.isEmpty {
                        ProjectIcon(projectId: session.projectId, name: session.project, image: projectIcon, size: 14)
                        Text(session.project).foregroundStyle(Theme.secondary)
                    }
                    if !session.branch.isEmpty && session.branch != session.title {
                        if showsProject && !session.project.isEmpty {
                            Text("·").foregroundStyle(Theme.tertiary)
                        }
                        Text(session.branch)
                            .font(Theme.monoSmall)
                            .foregroundStyle(Theme.tertiary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Spacer(minLength: 4)
                    ForEach(session.prs.sorted { $0.state.rank < $1.state.rank }.prefix(2), id: \.number) {
                        PRBadge(pr: $0)
                    }
                    if session.prs.count > 2 {
                        Text("+\(session.prs.count - 2)").foregroundStyle(Theme.tertiary)
                    }
                    Image(systemName: "server.rack").imageScale(.small).foregroundStyle(Theme.tertiary)
                    Text(session.machineOffline ? "\(session.machine) offline" : session.machine)
                        .foregroundStyle(session.machineOffline ? Theme.failure : Theme.tertiary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                .font(.caption)
            }
        }
        .padding(.vertical, 8)
        .contentShape(.rect)
    }
}

/// A session being written and not sent yet, laid out as a session row but dashed and quiet,
/// so it reads as a draft: a click opens it, the cross drops it.
struct DraftCard: View {
    let draft: Draft
    let fleet: Fleet
    /// The prompt as typed so far.
    let text: String
    /// Whether it is the draft open beside the list.
    let open: Bool
    let select: () -> Void
    let drop: () -> Void

    var body: some View {
        let prompt = text.trimmingCharacters(in: .whitespacesAndNewlines)
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "pencil")
                .font(.system(size: 10, weight: .bold))
                .foregroundStyle(Theme.tertiary)
                .frame(width: 10, height: 10)
                .padding(.top, 4)
            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(prompt.isEmpty ? "New session" : prompt.prefix { !$0.isNewline })
                        .font(.body.weight(.semibold))
                        .foregroundStyle(prompt.isEmpty ? Theme.tertiary : Theme.secondary)
                        .lineLimit(1)
                    Spacer(minLength: 4)
                    Text("Draft")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Theme.tertiary)
                    Button(action: drop) {
                        Image(systemName: "xmark")
                            .font(.caption.weight(.bold))
                            .foregroundStyle(Theme.tertiary)
                            .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .help("Drop Draft")
                    .accessibilityLabel("Drop draft")
                }
                Text("Not sent yet")
                    .font(.subheadline)
                    .italic()
                    .foregroundStyle(Theme.tertiary)
                if !project.isEmpty {
                    Text(project)
                        .font(.caption)
                        .foregroundStyle(Theme.tertiary)
                        .lineLimit(1)
                }
            }
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface.opacity(open ? 1 : 0.5), in: .rect(cornerRadius: Theme.corner))
        .overlay {
            RoundedRectangle(cornerRadius: Theme.corner)
                .strokeBorder(open ? Theme.secondary : Theme.stroke, style: StrokeStyle(lineWidth: 1.5, dash: [5, 4]))
        }
        .contentShape(.rect)
        .onTapGesture(perform: select)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(prompt.isEmpty ? "New session draft" : "Draft: \(prompt)")
        .accessibilityAddTraits(.isButton)
        .accessibilityAction { select() }
        .accessibilityAction(named: "Drop Draft") { drop() }
    }

    /// The draft's project, or the folder of its path.
    private var project: String {
        fleet.lists.projects.first { $0.id == draft.projectId }?.name
            ?? draft.repo.map { URL(fileURLWithPath: $0).lastPathComponent } ?? ""
    }
}

/// The elbow that ties a child session, or a provider agent, to its parent.
struct TreeLine: View {
    var body: some View {
        GeometryReader { geometry in
            Path { path in
                let x = geometry.size.width - 6
                path.move(to: CGPoint(x: x, y: -8))
                path.addLine(to: CGPoint(x: x, y: 11))
                path.addLine(to: CGPoint(x: geometry.size.width + 2, y: 11))
            }
            .stroke(Theme.tertiary, lineWidth: 1)
        }
    }
}

extension PrState {
    /// Open and draft PRs first, as the TUI lists them.
    var rank: Int {
        switch self {
        case .open, .draft: 0
        case .merged, .closed: 1
        }
    }
}
