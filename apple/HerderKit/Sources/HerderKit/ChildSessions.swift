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

/// What a child reported when it ended a turn: who, where the child stands now, and its message
/// as Markdown, folded to a few lines; the header opens the child. A report the same child has
/// since followed with another shows as one quiet line, which opens it in place.
struct ChildReportCard: View {
    let report: ChildReport
    let superseded: Bool
    /// The block's id, which keeps a superseded report open.
    let id: String
    let fleet: Fleet
    let hostId: HostId
    let open: ((SessionKey) -> Void)?
    @Environment(\.transcriptExpanded) private var opened

    var body: some View {
        if superseded {
            let shown = opened.wrappedValue.contains(id)
            VStack(alignment: .leading, spacing: 8) {
                EarlierReportLine(report: report, child: fleet.sessions[key], shown: shown) {
                    if shown { opened.wrappedValue.remove(id) } else { opened.wrappedValue.insert(id) }
                }
                if shown { card }
            }
        } else {
            card
        }
    }

    private var key: SessionKey { SessionKey(hostId: hostId, sessionId: report.sessionId) }

    private var card: some View {
        ReportCard(report: report, key: key, child: fleet.sessions[key], open: open)
    }
}

/// An earlier report as one line: "Earlier report", the child's title and the report's first
/// line, with a chevron.
private struct EarlierReportLine: View {
    let report: ChildReport
    let child: SessionModel?
    let shown: Bool
    let toggle: () -> Void

    var body: some View {
        let title = child?.title ?? "Session …\(report.sessionId.suffix(6))"
        Button(action: toggle) {
            HStack(spacing: 6) {
                ChildAvatar(session: child, size: 18, showsState: false)
                Text("Earlier report").foregroundStyle(Theme.secondary).fixedSize()
                Text("·").foregroundStyle(Theme.tertiary)
                Text(title).foregroundStyle(Theme.secondary).lineLimit(1).layoutPriority(1)
                Text("·").foregroundStyle(Theme.tertiary)
                Text(report.preview).foregroundStyle(Theme.tertiary).lineLimit(1).truncationMode(.tail)
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(Theme.tertiary)
                    .rotationEffect(.degrees(shown ? 90 : 0))
            }
            .font(.footnote)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .help("Earlier report from \(title)")
        .accessibilityLabel("Earlier report from \(title): \(report.preview)")
        .accessibilityValue(shown ? "Expanded" : "Collapsed")
    }
}

/// A report in full: a header that opens the child, then its message, folded with Show more.
private struct ReportCard: View {
    let report: ChildReport
    let key: SessionKey
    let child: SessionModel?
    let open: ((SessionKey) -> Void)?
    @State private var expanded = false
    @State private var height: CGFloat = 0
    @State private var hovering = false
    private let folded: CGFloat = 132

    var body: some View {
        let overflows = height > folded + 24
        let title = child?.title ?? "Session …\(report.sessionId.suffix(6))"
        let status = ReportStatus(progress: child?.progress, prs: child?.prs ?? [], end: child?.turnEnds[report.turnId])
        VStack(alignment: .leading, spacing: 10) {
            SessionButton(key: key, open: open) {
                HStack(spacing: 8) {
                    ChildAvatar(session: child, size: 24)
                    // The status follows the title, or goes under it where a phone has no room.
                    ViewThatFits(in: .horizontal) {
                        HStack(spacing: 8) {
                            titleText(title).fixedSize()
                            ReportStatusLine(status: status)
                        }
                        VStack(alignment: .leading, spacing: 2) {
                            titleText(title)
                            ReportStatusLine(status: status)
                        }
                    }
                    Spacer(minLength: 8)
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.bold))
                        .foregroundStyle(hovering ? Theme.secondary : Theme.tertiary)
                }
                .padding(.horizontal, 6)
                .padding(.vertical, 4)
                .background(hovering ? Theme.raised : .clear, in: .rect(cornerRadius: 7))
                .padding(.horizontal, -6)
                .padding(.vertical, -4)
                .contentShape(.rect)
                .onHover { hovering = $0 }
            }
            .help("Open the child session")
            .accessibilityLabel(status.text.isEmpty ? "Open child session: \(title)"
                                : "Open child session: \(title), \(status.text)")
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

    private func titleText(_ title: String) -> some View {
        Text(title).fontWeight(.semibold).foregroundStyle(Theme.text).lineLimit(1).truncationMode(.middle)
    }
}

/// A report's status after the child's title: a badge when the turn failed or was interrupted,
/// the child's state, then its PR.
private struct ReportStatusLine: View {
    let status: ReportStatus

    var body: some View {
        HStack(spacing: 5) {
            if let ending = status.ending {
                let color = ending == .failed ? Theme.failure : Theme.secondary
                Text(ending == .failed ? "Failed" : "Interrupted")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(color)
                    .padding(.horizontal, 7)
                    .padding(.vertical, 2)
                    .background(color.opacity(0.14), in: .capsule)
            }
            if let word = status.stateWord, let progress = status.progress {
                Text(word).foregroundStyle(progress == .idle || progress == .done ? Theme.secondary : progress.color)
            }
            if let text = status.prText, let pr = status.pr {
                if status.stateWord != nil { Text("·").foregroundStyle(Theme.tertiary) }
                Text(text).foregroundStyle(status.prAlarm ? Theme.failure : pr.state.color)
            }
        }
        .font(.caption)
        .lineLimit(1)
        .fixedSize()
    }
}

/// Where a reporting child stands now, rather than how its turn ended: its state and its most
/// pressing PR, as "Idle · #1922 draft", "Working" or "Idle · #1921 CI running". How the
/// reported turn ended shows only when it failed or was interrupted.
struct ReportStatus: Equatable {
    /// How the reported turn ended, when it did not complete.
    let ending: TurnEnd?
    /// The child's state; `nil` for a child this device does not know.
    let progress: ChildProgress?
    /// The child's most pressing PR: open, then draft, merged and closed, newest first.
    let pr: PullRequest?
    /// How many more PRs the child has.
    let more: Int

    init(progress: ChildProgress?, prs: [PullRequest], end: TurnEnd?) {
        ending = end == .failed || end == .interrupted ? end : nil
        self.progress = progress
        let prs = prs.sorted(by: PRRollup.order)
        pr = prs.first
        more = max(prs.count - 1, 0)
    }

    /// The child's state in a word; a failed child's is left out next to its failed turn.
    var stateWord: String? {
        switch progress {
        case nil: nil
        case .running: "Working"
        case .waiting: "Waiting"
        case .needsYou: "Needs you"
        case .idle, .done: "Idle"
        case .failed: ending == .failed ? nil : "Failed"
        case .archived: "Archived"
        case .moved: "Moved"
        }
    }

    /// The PR's number and what holds it up, or where it ended: "#1921 CI failing", "#1922 draft".
    var prText: String? {
        guard let pr else { return nil }
        let state: String = switch pr.state {
        case .draft: "draft"
        case .merged: "merged"
        case .closed: "closed"
        case .open:
            if pr.ci == .failing { "CI failing" }
            else if pr.mergeable == .conflicting { "conflicts" }
            else if pr.review == .changesRequested { "changes requested" }
            else if pr.ci == .pending { "CI running" }
            else if pr.review == .approved { "approved" }
            else { "open" }
        }
        return "#\(pr.number) \(state)" + (more > 0 ? " +\(more)" : "")
    }

    /// Whether the PR is held up by something that needs fixing.
    var prAlarm: Bool {
        guard let pr, pr.state == .open else { return false }
        return pr.ci == .failing || pr.mergeable == .conflicting || pr.review == .changesRequested
    }

    /// All of it on one line, as accessibility reads it.
    var text: String {
        [ending.map { $0 == .failed ? "Failed" : "Interrupted" }, stateWord, prText].compactMap { $0 }
            .joined(separator: " · ")
    }
}

extension ChildReport {
    /// What stands for an earlier report: its first line past any headings, which name a
    /// section rather than say anything; a heading's own text when there is nothing else.
    var preview: String {
        let lines = summary.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        return lines.first { Self.heading($0) == nil } ?? lines.first.map { Self.heading($0) ?? $0 } ?? ""
    }

    /// A Markdown heading's text: one to six `#`s, then a space; `#1922` is not one.
    private static func heading(_ line: String) -> String? {
        let marks = line.prefix { $0 == "#" }.count
        guard (1...6).contains(marks), line.dropFirst(marks).first == " " else { return nil }
        return line.dropFirst(marks).trimmingCharacters(in: .whitespaces)
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
