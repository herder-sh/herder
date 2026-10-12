import Herder
import SwiftUI

/// What a tab's navigation stack shows past its root, on iPhone.
enum NavRoute: Hashable {
    case session(SessionKey)
    /// A project's own screen, by its group's id.
    case project(String)
}

extension EnvironmentValues {
    /// The navigation path a session was pushed on, on iPhone; a child pops back to its parent.
    @Entry var sessionPath: Binding<[NavRoute]>?
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
            NavigationLink(value: NavRoute.session(key)) { label }.buttonStyle(.plain)
        }
    }
}

/// The child sessions the agent spawned, live, as T3 Code lists sub-agents: each one's provider,
/// model and state, what it does now and how long it has run. A row opens the child.
struct ChildrenCard: View {
    let children: [ChildRef]
    let fleet: Fleet
    let hostId: HostId
    let open: ((SessionKey) -> Void)?

    private func key(_ child: ChildRef) -> SessionKey { SessionKey(hostId: hostId, sessionId: child.sessionId) }

    private var progress: [ChildProgress] { children.map { fleet.sessions[key($0)]?.progress ?? .idle } }

    var body: some View {
        let progress = progress
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                HStack(spacing: -6) {
                    ForEach(children.prefix(3), id: \.sessionId) { child in
                        ChildAvatar(session: fleet.sessions[key(child)], size: 22, showsState: false)
                    }
                }
                VStack(alignment: .leading, spacing: 1) {
                    Text(children.count == 1 ? "Spawned 1 agent" : "Spawned \(children.count) agents")
                        .font(.footnote.weight(.semibold)).foregroundStyle(Theme.text)
                    Text(ChildProgress.summary(progress))
                        .font(.caption)
                        .foregroundStyle(progress.contains(.running) ? Theme.running
                                         : progress.contains(.needsYou) ? Theme.accent
                                         : progress.contains(.failed) ? Theme.failure : Theme.tertiary)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            VStack(spacing: 0) {
                ForEach(children, id: \.sessionId) { child in
                    SessionButton(key: key(child), open: open) {
                        ChildRow(child: child, session: fleet.sessions[key(child)])
                    }
                    .accessibilityLabel("Open child session: \(fleet.sessions[key(child)]?.title ?? child.task)")
                }
            }
            .padding(4)
            .background(Theme.background.opacity(0.5), in: .rect(cornerRadius: Theme.corner - 2))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner - 2).strokeBorder(Theme.stroke.opacity(0.7)))
            .padding([.horizontal, .bottom], 6)
        }
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// One spawned child: its provider, title and model, its state and the last thing it did on
/// one line, and its run time.
private struct ChildRow: View {
    let child: ChildRef
    let session: SessionModel?
    @State private var hovering = false

    var body: some View {
        let progress = session?.progress ?? .idle
        let title = session?.title ?? child.task
        // A failed child says why; an idle one, the last thing it said.
        let activity = progress == .failed ? session?.failure.map(firstLine) ?? "" : session?.activity ?? ""
        let detail = activity == progress.label || activity == "Idle" ? "" : activity
        HStack(spacing: 10) {
            ChildAvatar(session: session, size: 26)
            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(title).font(.footnote.weight(.semibold)).foregroundStyle(Theme.text).lineLimit(1)
                    if let session {
                        Text(ModelCatalog.name(session.model ?? "", provider: session.provider))
                            .font(.caption).foregroundStyle(Theme.tertiary).lineLimit(1)
                            .layoutPriority(-1)
                    }
                }
                HStack(spacing: 5) {
                    Text(progress.label).foregroundStyle(progress.color).fixedSize()
                    if !detail.isEmpty {
                        Text("·").foregroundStyle(Theme.tertiary)
                        Text(detail).foregroundStyle(progress == .failed ? Theme.failure : Theme.secondary)
                            .lineLimit(1).truncationMode(.tail)
                    }
                }
                .font(.caption)
            }
            Spacer(minLength: 8)
            if let session { RunTime(session: session) }
            Image(systemName: "chevron.right").font(.caption2.weight(.bold)).foregroundStyle(Theme.tertiary)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 7)
        .background(hovering ? Theme.raised : .clear, in: .rect(cornerRadius: 7))
        .contentShape(.rect)
        .onHover { hovering = $0 }
        .help(child.task)
    }
}

/// A child's identity: its provider's mark in a round tile, with its state as a dot.
struct ChildAvatar: View {
    let session: SessionModel?
    var size: CGFloat = 26
    var showsState = true

    var body: some View {
        let progress = session?.progress ?? .idle
        ProviderMark(provider: session?.provider ?? "", size: size * 0.54)
            .frame(width: size, height: size)
            .background(Theme.raised, in: Circle())
            .overlay(Circle().strokeBorder(Theme.stroke))
            .overlay(alignment: .bottomTrailing) {
                if showsState {
                    StateDot(progress: progress, size: max(7, size * 0.32)).offset(x: 1, y: 1)
                }
            }
            .background(Theme.surface, in: Circle().inset(by: -2))
            .accessibilityHidden(true)
    }
}

/// A child's state as a dot ringed in the background; a running child's pulses.
private struct StateDot: View {
    let progress: ChildProgress
    let size: CGFloat
    @State private var pulsing = false

    var body: some View {
        ZStack {
            if progress == .running {
                Circle().fill(Theme.running.opacity(0.4))
                    .scaleEffect(pulsing ? 2 : 1)
                    .opacity(pulsing ? 0 : 1)
                    .animation(.easeOut(duration: 1.4).repeatForever(autoreverses: false), value: pulsing)
            }
            Circle().fill(progress == .idle ? Theme.idle : progress.color)
        }
        .frame(width: size, height: size)
        .overlay(Circle().strokeBorder(Theme.surface, lineWidth: 2).padding(-2))
        .onAppear { pulsing = true }
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
        .font(.caption.monospaced())
        .foregroundStyle(Theme.tertiary)
    }

    private func text(at now: Date) -> some View {
        Text(Duration.seconds(session.runTime(at: now).rounded())
            .formatted(.units(allowed: [.hours, .minutes, .seconds], width: .narrow, maximumUnitCount: 2)))
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
                ChildAvatar(session: child, size: 24, showsState: false)
                VStack(alignment: .leading, spacing: 1) {
                    HStack(spacing: 6) {
                        Text(child?.title ?? "Session …\(report.sessionId.suffix(6))")
                            .fontWeight(.semibold).foregroundStyle(Theme.text).lineLimit(1).truncationMode(.middle)
                        ReportBadge(end: child?.turnEnds[report.turnId])
                    }
                    Text(child.map { "Report · \(ModelCatalog.name($0.model ?? "", provider: $0.provider))" } ?? "Report")
                        .font(.caption).foregroundStyle(Theme.tertiary).lineLimit(1)
                }
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
        .messageCopy(report.summary)
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

/// What marks a child session, above its header and so in view however far it scrolls: whose
/// agent it is, in the child tint, and the way back to the parent.
struct ChildBanner: View {
    let fleet: Fleet
    let parent: SessionKey
    let open: ((SessionKey) -> Void)?
    @Environment(\.sessionPath) private var path

    private var listed: Bool {
        fleet.machines.first { $0.hostId == parent.hostId }?.sessions.contains { $0.sessionId == parent.sessionId } == true
    }

    var body: some View {
        let name = fleet.sessions[parent]?.title ?? "Session …\(parent.sessionId.suffix(6))"
        HStack(spacing: 10) {
            if listed {
                Group {
                    if let open {
                        Button { open(parent) } label: { back }
                            .keyboardShortcut("[", modifiers: .command)
                    } else if let path, path.wrappedValue.dropLast().last == .session(parent) {
                        // Pushed from the parent: back pops to it, where it was left.
                        Button { path.wrappedValue.removeLast() } label: { back }
                    } else {
                        NavigationLink(value: NavRoute.session(parent)) { back }
                    }
                }
                .buttonStyle(.plain)
                .help("Back to \(name) (⌘[)")
                .accessibilityLabel("Back to parent session \(name)")
            }
            HStack(spacing: 5) {
                Image(systemName: "arrow.turn.down.right").foregroundStyle(Theme.child)
                Text("Agent of").foregroundStyle(Theme.child)
                Text(name).foregroundStyle(Theme.text).lineLimit(1).truncationMode(.middle)
                if !listed {
                    Text("· not listed").foregroundStyle(Theme.tertiary).fixedSize()
                }
            }
            .accessibilityElement(children: .combine)
            Spacer(minLength: 0)
        }
        .font(.caption.weight(.semibold))
        .padding(.horizontal, 20)
        .frame(minHeight: 38)
        .background(Theme.child.opacity(0.09))
        .overlay(alignment: .bottom) { Rectangle().fill(Theme.child.opacity(0.22)).frame(height: 1) }
    }

    private var back: some View {
        HStack(spacing: 3) {
            Image(systemName: "chevron.left").font(.caption2.weight(.bold))
            Text("Back")
        }
        .foregroundStyle(Theme.text)
        .padding(.horizontal, 8)
        .frame(height: 24)
        .background(Theme.child.opacity(0.16), in: .rect(cornerRadius: 6))
        .contentShape(.rect)
    }
}

extension ChildProgress {
    /// The states of a set of children, most urgent first: "1 needs you · 2 running · 1 done".
    static func summary(_ states: [ChildProgress]) -> String {
        let order: [ChildProgress] = [.needsYou, .running, .waiting, .failed, .done, .idle, .archived, .moved]
        return order.compactMap { state in
            let count = states.filter { $0 == state }.count
            guard count > 0 else { return nil }
            return "\(count) \(state == .waiting ? "waiting" : state.label.lowercased())"
        }
        .joined(separator: " · ")
    }

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
