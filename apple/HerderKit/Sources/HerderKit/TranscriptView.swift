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
        case .user(_, let text, let attachments, let outgoing, let agentMessage):
            VStack(alignment: .trailing, spacing: 6) {
                if let agentMessage {
                    AgentMessageSource(message: agentMessage, fleet: fleet, hostId: hostId, open: open)
                }
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
        case .report(let report):
            ChildReportCard(report: report, fleet: fleet, hostId: hostId, open: open)
        case .notice(let notice):
            NoticeLine(notice: notice)
        case .handoff(let handoff):
            HandoffDivider(handoff: handoff, accounts: fleet.machines.first { $0.hostId == hostId }?.accounts ?? [],
                           machineName: { fleet.machineName($0, of: key) })
        }
    }
}

/// An event as a line across the transcript; one with more to it shows the rest on hover, and
/// in full on a click.
struct NoticeLine: View {
    let notice: Notice
    @State private var expanded = false

    var body: some View {
        HStack(spacing: 8) {
            Rectangle().fill(Theme.stroke).frame(height: 1)
            Text(expanded ? notice.detail ?? notice.text : notice.text)
                .font(.caption.weight(.medium))
                .foregroundStyle(notice.tone == .error ? Theme.failure : notice.tone == .attention ? Theme.accent : Theme.tertiary)
                .multilineTextAlignment(.center)
                .textSelection(.enabled)
                .layoutPriority(1)
                .help(notice.detail ?? "")
                .onTapGesture { if notice.detail != nil { expanded.toggle() } }
            Rectangle().fill(Theme.stroke).frame(height: 1)
        }
    }
}

extension Fleet {
    /// What to call a machine: its name, else the name a vault lists it under, else its short id.
    /// Through a vault, the session `key` runs on the host the vault lists it under.
    func machineName(_ id: HostId?, of key: SessionKey) -> String {
        guard var id else { return "?" }
        if id == key.hostId, let runsOn = machines.first(where: { $0.hostId == id })?
            .sessions.first(where: { $0.sessionId == key.sessionId })?.hostId {
            id = runsOn
        }
        if let machine = machines.first(where: { $0.hostId == id }) { return machine.name }
        if let host = machines.lazy.flatMap(\.hosts).first(where: { $0.hostId == id }) { return host.hostName }
        return String(id.suffix(6))
    }
}

/// A handoff, as one line across the transcript: what the session ran on, then what it runs on
/// from here, whatever moved it.
struct HandoffDivider: View {
    let handoff: Handoff
    let accounts: [Account]
    /// Names a machine of a move between machines.
    let machineName: (HostId?) -> String

    var body: some View {
        HStack(spacing: 12) {
            Rectangle().fill(Theme.stroke).frame(height: 1)
            HStack(spacing: 8) {
                Label("Handoff", systemImage: "arrow.left.arrow.right").foregroundStyle(Theme.tertiary)
                HandoffSide(handoff: handoff, side: handoff.from, accounts: accounts, machineName: machineName)
                    .foregroundStyle(Theme.secondary)
                Image(systemName: "arrow.right").foregroundStyle(Theme.tertiary)
                HandoffSide(handoff: handoff, side: handoff.to, accounts: accounts, machineName: machineName)
                    .foregroundStyle(Theme.accent)
            }
            .font(.caption.weight(.medium))
            .lineLimit(1)
            .layoutPriority(1)
            Rectangle().fill(Theme.stroke).frame(height: 1)
        }
        .padding(.vertical, 6)
    }
}

/// A side of a handoff: a move between machines names the machines; an account switch, the
/// accounts; the others, the models.
struct HandoffSide: View {
    let handoff: Handoff
    let side: Handoff.Side
    let accounts: [Account]
    let machineName: (HostId?) -> String

    var body: some View {
        HStack(spacing: 5) {
            if handoff.kind == .machine {
                Image(systemName: "desktopcomputer")
                Text(machineName(side.hostId))
            } else if handoff.kind == .account {
                Image(systemName: "person.crop.circle")
                Text(account(side.accountId))
            } else {
                if let provider = side.provider { ProviderMark(provider: provider, size: 12) }
                Text(ModelCatalog.name(side.model ?? "", provider: side.provider))
            }
        }
    }

    private func account(_ id: AccountId?) -> String {
        guard let id else { return "?" }
        return accounts.first { $0.accountId == id }?.label ?? id
    }
}

/// Assistant prose: inline Markdown, headings, bullets, quotes, tables and fenced code.
struct MarkdownText: View {
    let text: String
    var streaming = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(parts.enumerated()), id: \.offset) { index, part in
                switch part {
                case .code(let code, let language):
                    Self.codeText(code, language: language)
                        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
                        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
                case .diagram(let source):
                    MermaidBlock(source: source)
                case .table(let table):
                    MarkdownTableView(table: table)
                case .line(let line):
                    lineView(line, last: index == parts.count - 1)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    static func codeText(_ code: String, language: String) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Text(CodeHighlight.attributed(code, language: language)).font(Theme.mono).foregroundStyle(Theme.text).textSelection(.enabled)
                .padding(12)
        }
    }

    @ViewBuilder private func lineView(_ line: String, last: Bool) -> some View {
        let cursor = streaming && last ? " ▍" : ""
        if line.hasPrefix("#") {
            Text(Self.inline(line.drop { $0 == "#" }.trimmingCharacters(in: .whitespaces) + cursor))
                .font(.headline).foregroundStyle(Theme.text)
        } else if let item = ["- ", "* ", "+ "].first(where: line.hasPrefix) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("•").foregroundStyle(Theme.secondary)
                Text(Self.inline(String(line.dropFirst(item.count)) + cursor))
            }
            .font(.body).foregroundStyle(Theme.text)
        } else if line.hasPrefix(">") {
            Text(Self.inline(line.dropFirst().trimmingCharacters(in: .whitespaces) + cursor))
                .italic().foregroundStyle(Theme.secondary)
                .padding(.leading, 10)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.stroke).frame(width: 2) }
        } else {
            Text(Self.inline(line + cursor)).font(.body).foregroundStyle(Theme.text).lineSpacing(3)
        }
    }

    static func inline(_ text: String) -> AttributedString {
        var string = (try? AttributedString(markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(text)
        autolink(&string)
        return string
    }

    /// Links bare `http(s)://` URLs, which Markdown leaves as text, outside code spans and links.
    /// Scheme-less matches stay text: file names like `main.rs` would read as domains.
    static func autolink(_ string: inout AttributedString) {
        let plain = String(string.characters)
        guard let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) else { return }
        for match in detector.matches(in: plain, range: NSRange(plain.startIndex..., in: plain)) {
            guard let url = match.url, let range = Range(match.range, in: plain),
                  ["http://", "https://"].contains(where: plain[range].lowercased().hasPrefix) else { continue }
            let start = string.characters.index(string.startIndex, offsetBy: plain.distance(from: plain.startIndex, to: range.lowerBound))
            let end = string.characters.index(start, offsetBy: plain.distance(from: range.lowerBound, to: range.upperBound))
            let linked = string[start..<end].runs.contains {
                $0.link != nil || $0.inlinePresentationIntent?.contains(.code) == true
            }
            if !linked { string[start..<end].link = url }
        }
    }

    enum Part: Equatable {
        case line(String), code(String, language: String), diagram(String), table(MarkdownTable)
    }

    /// Fenced code blocks, pipe tables, and the non-blank lines between them. A ```mermaid
    /// fence becomes a diagram once it's closed, so a streaming one stays source until then.
    private var parts: [Part] { Self.parse(text, streaming: streaming) }

    static func parse(_ text: String, streaming: Bool = false) -> [Part] {
        var language = ""
        var parts: [Part] = []
        var code: [Substring]?
        var table: MarkdownTable?
        // The line before this one, since a table's separator must follow its header directly.
        var previous: String?
        for line in text.split(separator: "\n", omittingEmptySubsequences: false) {
            defer { previous = String(line) }
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if code == nil, table != nil, trimmed.contains("|"), !trimmed.hasPrefix("```") {
                table?.append(String(line))
                continue
            }
            if let open = table {
                parts.append(.table(open))
                table = nil
            }
            if trimmed.hasPrefix("```") {
                if let lines = code {
                    let block = lines.joined(separator: "\n")
                    let diagram = language.lowercased() == "mermaid" && !block.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    parts.append(diagram ? .diagram(block) : .code(block, language: language))
                    code = nil
                } else {
                    language = String(line.trimmingCharacters(in: .whitespaces).dropFirst(3))
                        .split(whereSeparator: \.isWhitespace).first.map(String.init) ?? ""
                    code = []
                }
            } else if code != nil {
                code?.append(line)
            } else if case .line(let header)? = parts.last, header == previous,
                      let start = MarkdownTable(header: header, separator: trimmed) {
                parts.removeLast()
                table = start
            } else if !trimmed.isEmpty {
                parts.append(.line(String(line)))
            }
        }
        if let open = table { parts.append(.table(open)) }
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
