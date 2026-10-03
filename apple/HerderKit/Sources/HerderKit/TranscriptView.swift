import Herder
import SwiftUI

/// One block of a transcript: prose in focus, tool calls as quiet one-line rows.
struct TranscriptBlockView: View {
    let block: TranscriptBlock
    let fleet: Fleet
    let key: SessionKey
    /// Opens a child session; `nil` pushes it.
    let open: ((SessionKey) -> Void)?
    private var hostId: HostId { key.hostId }

    var body: some View {
        switch block {
        case .user(_, let text, let attachments, let outgoing):
            VStack(alignment: .trailing, spacing: 6) {
                if !attachments.isEmpty {
                    MessageImages(fleet: fleet, key: key, attachments: attachments)
                }
                if let outgoing, !outgoing.images.isEmpty {
                    HStack(spacing: 8) {
                        ForEach(Array(outgoing.images.enumerated()), id: \.offset) { _, data in Picture(data: data, height: 140) }
                    }
                }
                Text(text)
                    .font(.body)
                    .foregroundStyle(Theme.onBubble)
                    .textSelection(.enabled)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .background(Theme.bubble.opacity(outgoing == nil ? 1 : 0.6), in: .rect(cornerRadius: 18))
                if let outgoing {
                    DeliveryLine(outgoing: outgoing) {
                        fleet.discard(outgoing, from: key)
                        Task { await fleet.submit(outgoing.text, to: key) }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
            .padding(.leading, 48)
        case .working(let since, let waiting):
            WorkingLine(since: since, waiting: waiting)
        case .assistant(_, let text, let streaming):
            MarkdownText(text: text, streaming: streaming)
        case .reasoning(_, let text, let streaming):
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "brain").foregroundStyle(Theme.tertiary)
                Text(text + (streaming ? " ▍" : "")).italic().foregroundStyle(Theme.secondary)
                    .lineLimit(streaming ? nil : 3)
            }
            .font(.footnote)
            .frame(maxWidth: .infinity, alignment: .leading)
        case .tools(_, let calls):
            ToolGroup(calls: calls)
        case .agents(_, let agents):
            NativeAgentGroup(agents: agents, fleet: fleet, key: key)
        case .children(_, let children):
            ChildrenCard(children: children, fleet: fleet, hostId: hostId, open: open)
        case .notice(let notice):
            HStack(spacing: 8) {
                Rectangle().fill(Theme.stroke).frame(height: 1)
                Text(notice.text)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(notice.tone == .error ? Theme.failure : notice.tone == .attention ? Theme.accent : Theme.tertiary)
                    .multilineTextAlignment(.center)
                    .layoutPriority(1)
                Rectangle().fill(Theme.stroke).frame(height: 1)
            }
        }
    }
}

/// Assistant prose: inline Markdown, headings, bullets, quotes and fenced code.
struct MarkdownText: View {
    let text: String
    var streaming = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(parts.enumerated()), id: \.offset) { index, part in
                switch part {
                case .code(let code, let language):
                    ScrollView(.horizontal, showsIndicators: false) {
                        Text(CodeHighlight.attributed(code, language: language)).font(Theme.mono).foregroundStyle(Theme.text).textSelection(.enabled)
                            .padding(12)
                    }
                    .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
                    .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
                case .line(let line):
                    lineView(line, last: index == parts.count - 1)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder private func lineView(_ line: String, last: Bool) -> some View {
        let cursor = streaming && last ? " ▍" : ""
        if line.hasPrefix("#") {
            Text(inline(line.drop { $0 == "#" }.trimmingCharacters(in: .whitespaces) + cursor))
                .font(.headline).foregroundStyle(Theme.text)
        } else if let item = ["- ", "* ", "+ "].first(where: line.hasPrefix) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("•").foregroundStyle(Theme.secondary)
                Text(inline(String(line.dropFirst(item.count)) + cursor))
            }
            .font(.body).foregroundStyle(Theme.text)
        } else if line.hasPrefix(">") {
            Text(inline(line.dropFirst().trimmingCharacters(in: .whitespaces) + cursor))
                .italic().foregroundStyle(Theme.secondary)
                .padding(.leading, 10)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.stroke).frame(width: 2) }
        } else {
            Text(inline(line + cursor)).font(.body).foregroundStyle(Theme.text).lineSpacing(3)
        }
    }

    private func inline(_ text: String) -> AttributedString {
        (try? AttributedString(markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(text)
    }

    enum Part: Equatable { case line(String), code(String, language: String) }

    /// Fenced code blocks, and the non-blank lines between them.
    private var parts: [Part] { Self.parse(text, streaming: streaming) }

    static func parse(_ text: String, streaming: Bool = false) -> [Part] {
        var language = ""
        var parts: [Part] = []
        var code: [Substring]?
        for line in text.split(separator: "\n", omittingEmptySubsequences: false) {
            if line.trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                if let lines = code {
                    parts.append(.code(lines.joined(separator: "\n"), language: language))
                    code = nil
                } else {
                    language = String(line.trimmingCharacters(in: .whitespaces).dropFirst(3))
                        .split(whereSeparator: \.isWhitespace).first.map(String.init) ?? ""
                    code = []
                }
            } else if code != nil {
                code?.append(line)
            } else if !line.trimmingCharacters(in: .whitespaces).isEmpty {
                parts.append(.line(String(line)))
            }
        }
        if let lines = code { parts.append(.code(lines.joined(separator: "\n"), language: language)) }
        if parts.isEmpty && streaming { parts.append(.line("")) }
        return parts
    }
}

/// A run of tool calls as one quiet block.
private struct ToolGroup: View {
    let calls: [ToolCall]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(calls.enumerated()), id: \.element.id) { index, call in
                if index > 0 { Divider().overlay(Theme.stroke).padding(.leading, 38) }
                ToolRow(call: call)
            }
        }
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

private struct ToolRow: View {
    let call: ToolCall
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 10) {
                Image(systemName: symbol)
                    .font(.footnote.weight(.medium))
                    .foregroundStyle(Theme.secondary)
                    .frame(width: 16)
                Text(call.name).font(.caption.weight(.semibold)).foregroundStyle(Theme.secondary)
                Text(call.summary)
                    .font(Theme.mono)
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 6)
                if call.added + call.removed > 0 {
                    Text("+\(call.added)").foregroundStyle(Theme.success)
                    Text("−\(call.removed)").foregroundStyle(Theme.failure)
                }
                switch call.outcome {
                case .running: ProgressView().controlSize(.mini).tint(Theme.running)
                case .ok: Image(systemName: "checkmark").foregroundStyle(Theme.tertiary)
                case .failed: Image(systemName: "xmark").foregroundStyle(Theme.failure)
                case .unknown: EmptyView()
                }
            }
            .font(.caption.weight(.medium).monospacedDigit())
            if !call.output.isEmpty {
                Text(call.output + (call.moreLines > 0 ? "  (+\(call.moreLines) lines)" : ""))
                    .font(Theme.monoSmall)
                    .foregroundStyle(call.outcome == .failed ? Theme.failure : Theme.tertiary)
                    .lineLimit(1)
                    .padding(.leading, 26)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .frame(minHeight: 40)
    }

    private var symbol: String {
        switch call.kind {
        case .command: "terminal"
        case .edit: "pencil"
        case .read: "doc.text"
        case .search: "magnifyingglass"
        case .web: "globe"
        case .other: "gearshape"
        }
    }
}

/// The child sessions the agent spawned, with their state live; each opens its session.
private struct ChildrenCard: View {
    let children: [ChildRef]
    let fleet: Fleet
    let hostId: HostId
    let open: ((SessionKey) -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Image(systemName: "point.3.connected.trianglepath.dotted")
                Text(children.count == 1 ? "Spawned a child session" : "Spawned \(children.count) child sessions")
            }
            .font(.footnote.weight(.semibold))
            .foregroundStyle(Theme.text)
            ForEach(children, id: \.sessionId) { child in
                let key = SessionKey(hostId: hostId, sessionId: child.sessionId)
                let session = fleet.sessions[key]
                if let open {
                    Button { open(key) } label: { row(child, session) }.buttonStyle(.plain)
                } else {
                    NavigationLink(value: key) { row(child, session) }.buttonStyle(.plain)
                }
            }
        }
        .padding(12)
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }

    private func row(_ child: ChildRef, _ session: SessionModel?) -> some View {
        HStack(spacing: 8) {
            StatusGlyph(state: session?.state ?? .idle, size: 8)
            Text(child.task).foregroundStyle(Theme.text).lineLimit(1)
            Spacer()
            Text(session?.activity ?? "")
                .foregroundStyle(session?.state == .needsYou ? Theme.accent : Theme.tertiary)
                .lineLimit(1)
            Image(systemName: "chevron.right").foregroundStyle(Theme.tertiary)
        }
        .font(.footnote)
        .padding(.horizontal, 10)
        .frame(minHeight: 44)
        .background(Theme.raised, in: .rect(cornerRadius: Theme.corner - 2))
        .contentShape(.rect)
    }
}

/// Where a message from this device is: on its way, with the daemon, or refused.
private struct DeliveryLine: View {
    let outgoing: Outgoing
    let retry: () -> Void

    var body: some View {
        HStack(spacing: 6) {
            switch outgoing.state {
            case .sending:
                ProgressView().controlSize(.mini).tint(Theme.tertiary)
                Text("Sending…")
            case .delivered:
                EmptyView()
            case .failed(let reason):
                Image(systemName: "exclamationmark.triangle").foregroundStyle(Theme.failure)
                Text("Not sent: \(reason)").foregroundStyle(Theme.failure).lineLimit(2)
                Button("Retry", action: retry)
                    .buttonStyle(.plain)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Theme.text)
            }
        }
        .font(.caption)
        .foregroundStyle(Theme.tertiary)
    }
}

/// The agent at work, with how long the turn has run, or the wait for it to take a message.
private struct WorkingLine: View {
    let since: Date?
    let waiting: Bool

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            HStack(spacing: 10) {
                StatusGlyph(state: waiting ? .waiting : .running, size: 7)
                Text(waiting ? "Waiting for the agent to take it…" : "Working…")
                    .foregroundStyle(Theme.secondary)
                if let since {
                    Text(Duration.seconds(max(0, context.date.timeIntervalSince(since)))
                        .formatted(.time(pattern: .minuteSecond)))
                        .monospacedDigit()
                        .foregroundStyle(Theme.tertiary)
                }
            }
            .font(.footnote)
        }
    }
}
