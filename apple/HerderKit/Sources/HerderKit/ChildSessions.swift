import Herder
import SwiftUI

extension EnvironmentValues {
    /// The navigation path a session was pushed on, on iPhone; a child pops back to its parent.
    @Entry var sessionPath: Binding<[SessionKey]>?
}

/// Opens a session: through `open` beside the list, else by pushing it.
struct SessionButton<Label: View>: View {
    let key: SessionKey
    let open: ((SessionKey) -> Void)?
    @ViewBuilder let label: Label

    var body: some View {
        if let open {
            Button { open(key) } label: { label }.buttonStyle(.plain)
        } else {
            NavigationLink(value: key) { label }.buttonStyle(.plain)
        }
    }
}

/// The child sessions the agent spawned, live: each one's state, what it does now and how
/// long it has run. A row opens the child.
struct ChildrenCard: View {
    let children: [ChildRef]
    let fleet: Fleet
    let hostId: HostId
    let open: ((SessionKey) -> Void)?

    private func key(_ child: ChildRef) -> SessionKey { SessionKey(hostId: hostId, sessionId: child.sessionId) }

    /// "2 running, 1 done".
    private var summary: String {
        let progress = children.map { fleet.sessions[key($0)]?.progress ?? .idle }
        var counts: [(ChildProgress, Int)] = []
        for state in progress {
            if let index = counts.firstIndex(where: { $0.0 == state }) { counts[index].1 += 1 } else { counts.append((state, 1)) }
        }
        return counts.map { "\($0.1) \($0.0.label.lowercased())" }.joined(separator: ", ")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "point.3.connected.trianglepath.dotted").foregroundStyle(Theme.secondary)
                Text(children.count == 1 ? "Spawned a child session" : "Spawned \(children.count) child sessions")
                    .foregroundStyle(Theme.text)
                Spacer(minLength: 8)
                if children.count > 1 {
                    Text(summary).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                }
            }
            .font(.footnote.weight(.semibold))
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            ForEach(children, id: \.sessionId) { child in
                Divider().overlay(Theme.stroke)
                SessionButton(key: key(child), open: open) {
                    ChildRow(child: child, session: fleet.sessions[key(child)])
                }
                .accessibilityLabel("Open child session: \(fleet.sessions[key(child)]?.title ?? child.task)")
            }
        }
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// One spawned child: its state, title and task, the last thing it did, and its run time.
private struct ChildRow: View {
    let child: ChildRef
    let session: SessionModel?
    @State private var hovering = false

    var body: some View {
        let progress = session?.progress ?? .idle
        let title = session?.title ?? child.task
        // A failed child says why; an idle one, the last thing it said.
        let activity = progress == .failed ? session?.failure.map(firstLine) ?? "" : session?.activity ?? ""
        HStack(alignment: .top, spacing: 10) {
            StatusGlyph(state: progress.state, size: 8)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(title).font(.callout.weight(.semibold)).foregroundStyle(Theme.text).lineLimit(1)
                    Spacer(minLength: 8)
                    if let session { RunTime(session: session) }
                }
                if child.task != title {
                    Text(child.task).font(.caption).foregroundStyle(Theme.secondary).lineLimit(2)
                }
                HStack(spacing: 5) {
                    Text(progress.label).foregroundStyle(progress.color)
                    if !activity.isEmpty && activity != progress.label && activity != "Idle" {
                        Text("·").foregroundStyle(Theme.tertiary)
                        Text(activity).foregroundStyle(progress == .needsYou ? Theme.accent : Theme.tertiary)
                            .lineLimit(1).truncationMode(.tail)
                    }
                }
                .font(.caption.weight(.medium))
            }
            Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                .padding(.top, 3)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(hovering ? Theme.raised : .clear)
        .contentShape(.rect)
        .onHover { hovering = $0 }
    }
}

/// How long a session has spent in turns, ticking while one runs.
private struct RunTime: View {
    let session: SessionModel

    var body: some View {
        Group {
            if session.turn != nil {
                TimelineView(.periodic(from: .now, by: 1)) { context in text(at: context.date) }
            } else if session.runTime(at: .now) > 0 {
                text(at: .now)
            }
        }
        .font(.caption.monospacedDigit())
        .foregroundStyle(Theme.tertiary)
    }

    private func text(at now: Date) -> some View {
        Label(Duration.seconds(session.runTime(at: now).rounded())
            .formatted(.units(allowed: [.hours, .minutes, .seconds], width: .narrow, maximumUnitCount: 2)),
              systemImage: "clock")
            .labelStyle(.titleAndIcon)
            .help("Time spent working")
    }
}

/// What a child reported when it ended a turn: who, how the turn ended, and its message as
/// Markdown, folded to a few lines; the child is a click away.
struct ChildReportCard: View {
    let report: ChildReport
    let fleet: Fleet
    let hostId: HostId
    let open: ((SessionKey) -> Void)?
    @State private var expanded = false
    @State private var height: CGFloat = 0
    private let folded: CGFloat = 132

    private var key: SessionKey { SessionKey(hostId: hostId, sessionId: report.sessionId) }

    var body: some View {
        let child = fleet.sessions[key]
        let overflows = height > folded + 24
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                StatusGlyph(state: child?.progress.state ?? .idle, size: 7)
                Text("Report from").foregroundStyle(Theme.secondary)
                Text(child?.title ?? "Session …\(report.sessionId.suffix(6))")
                    .fontWeight(.semibold).foregroundStyle(Theme.text).lineLimit(1).truncationMode(.middle)
                ReportBadge(end: child?.turnEnds[report.turnId])
                Spacer(minLength: 8)
                SessionButton(key: key, open: open) {
                    HStack(spacing: 4) {
                        Text("Open")
                        Image(systemName: "arrow.up.right")
                    }
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Theme.text)
                    .padding(.horizontal, 10)
                    .frame(height: 26)
                    .background(Theme.raised, in: .rect(cornerRadius: 7))
                    .contentShape(.rect)
                }
                .help("Open the child session")
            }
            .font(.footnote)
            MarkdownText(text: report.summary)
                .fixedSize(horizontal: false, vertical: true)
                .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height = $0 }
                .frame(maxHeight: overflows && !expanded ? folded : nil, alignment: .top)
                .clipped()
                .mask {
                    LinearGradient(stops: [.init(color: .black, location: 0.7),
                                           .init(color: .black.opacity(overflows && !expanded ? 0 : 1), location: 1)],
                                   startPoint: .top, endPoint: .bottom)
                }
            if overflows {
                Button { withAnimation(.easeInOut(duration: 0.15)) { expanded.toggle() } } label: {
                    Label(expanded ? "Show less" : "Show more", systemImage: expanded ? "chevron.up" : "chevron.down")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Theme.secondary)
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// How the reported turn ended, as a small badge.
private struct ReportBadge: View {
    let end: TurnEnd?

    var body: some View {
        let (text, color): (String, Color) = switch end {
        case .completed: ("Done", Theme.success)
        case .failed: ("Failed", Theme.failure)
        case .interrupted: ("Interrupted", Theme.secondary)
        case nil: ("Reported", Theme.secondary)
        }
        Text(text)
            .font(.caption2.weight(.semibold))
            .foregroundStyle(color)
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(color.opacity(0.14), in: .capsule)
            .fixedSize()
    }
}

/// A child session's way back to its parent: "← Parent › Child session".
struct ParentLink: View {
    let fleet: Fleet
    let parent: SessionKey
    let open: ((SessionKey) -> Void)?
    @Environment(\.sessionPath) private var path

    private var listed: Bool {
        fleet.machines.first { $0.hostId == parent.hostId }?.sessions.contains { $0.sessionId == parent.sessionId } == true
    }

    var body: some View {
        let name = fleet.sessions[parent]?.title ?? "Session …\(parent.sessionId.suffix(6))"
        HStack(spacing: 6) {
            Group {
                if !listed {
                    crumb(name, symbol: "arrow.turn.left.up")
                } else if let open {
                    Button { open(parent) } label: { crumb(name, symbol: "chevron.left") }
                        .buttonStyle(.plain)
                        .keyboardShortcut("[", modifiers: .command)
                } else if let path, path.wrappedValue.dropLast().last == parent {
                    // Pushed from the parent: back pops to it, where it was left.
                    Button { path.wrappedValue.removeLast() } label: { crumb(name, symbol: "chevron.left") }
                        .buttonStyle(.plain)
                } else {
                    NavigationLink(value: parent) { crumb(name, symbol: "chevron.left") }.buttonStyle(.plain)
                }
            }
            .help(listed ? "Back to the parent session (⌘[)" : "The parent session is not listed")
            Image(systemName: "chevron.right").font(.caption2.weight(.bold)).foregroundStyle(Theme.tertiary)
            Text("Child session").foregroundStyle(Theme.tertiary).lineLimit(1)
        }
        .font(.caption.weight(.semibold))
        .accessibilityElement(children: .contain)
    }

    private func crumb(_ name: String, symbol: String) -> some View {
        HStack(spacing: 4) {
            Image(systemName: symbol)
            Text(name).lineLimit(1).truncationMode(.middle)
        }
        .foregroundStyle(listed ? Theme.text : Theme.secondary)
        .padding(.horizontal, 8)
        .frame(height: 24)
        .background(Theme.raised, in: .rect(cornerRadius: 6))
        .contentShape(.rect)
        .accessibilityLabel(listed ? "Back to parent session \(name)" : "Parent session \(name)")
    }
}

extension ChildProgress {
    var color: Color {
        switch self {
        case .running: Theme.running
        case .needsYou: Theme.accent
        case .done: Theme.success
        case .failed: Theme.failure
        case .waiting, .idle, .archived, .moved: Theme.secondary
        }
    }
}
