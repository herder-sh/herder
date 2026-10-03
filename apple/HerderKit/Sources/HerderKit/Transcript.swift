import Foundation
import Herder

/// A tool call with what came back.
struct ToolCall: Hashable, Identifiable {
    enum Kind: Hashable { case command, edit, read, search, web, other }
    enum Outcome: Hashable { case running, ok, failed, unknown }

    let id: ItemId
    let name: String
    let kind: Kind
    /// The input's telling value: the command, the path, the pattern.
    let summary: String
    var outcome: Outcome
    /// The first line of the output, and how many more there are.
    var output: String = ""
    var moreLines = 0
    var added = 0
    var removed = 0
}

/// A block of a session's transcript, as the session view shows it.
enum TranscriptBlock: Hashable, Identifiable {
    /// A user message; `outgoing` while it is on its way from this device.
    case user(id: String, text: String, attachments: [Attachment] = [], outgoing: Outgoing?)
    /// The agent is working, or about to, since the date.
    case working(since: Date?, waiting: Bool)
    case assistant(id: String, text: String, streaming: Bool)
    case reasoning(id: String, text: String, streaming: Bool)
    case tools(id: String, calls: [ToolCall])
    case children(id: String, [ChildRef])
    case notice(Notice)

    var id: String {
        switch self {
        case .user(let id, _, _, _), .assistant(let id, _, _), .reasoning(let id, _, _), .tools(let id, _),
             .children(let id, _): id
        case .working: "working"
        case .notice(let notice): "notice-\(notice.id)"
        }
    }

}

/// A child session the agent spawned.
struct ChildRef: Hashable {
    let sessionId: SessionId
    let task: String
}

enum Transcript {
    /// The session's transcript: completed entries, streaming content, and pending or failed sends.
    /// Consecutive tool calls and their results group into one block.
    static func blocks(_ model: SessionModel) -> [TranscriptBlock] {
        var blocks: [TranscriptBlock] = []
        var calls: [ToolCall] = []
        var callsTurn = ""
        var completedGroups: [TurnId: [ItemId: Int]] = [:]
        var children: [ChildRef] = []

        func flushCalls() {
            if let first = calls.first {
                for call in calls { completedGroups[callsTurn, default: [:]][call.id] = blocks.count }
                blocks.append(.tools(id: "tools-\(callsTurn)/\(first.id)", calls: calls))
            }
            calls = []
        }
        func flushChildren() {
            if let first = children.first { blocks.append(.children(id: "children-\(first.sessionId)", children)) }
            children = []
        }
        func add(_ item: Item, streaming: Bool) {
            // Adapters number items per turn, so an item id alone repeats across turns.
            let id = "\(item.turnId)/\(item.id)"
            switch item.body {
            case .toolCall(let name, let input):
                flushChildren()
                if callsTurn != item.turnId { flushCalls() }
                if calls.isEmpty { callsTurn = item.turnId }
                calls.append(toolCall(id: item.id, name: name, input: input, running: streaming || model.turn == item.turnId))
            case .toolResult(let callId, let output, let isError):
                if callsTurn == item.turnId, let index = calls.lastIndex(where: { $0.id == callId }) {
                    calls[index].attach(output: output, isError: isError, streaming: streaming)
                } else if let blockIndex = completedGroups[item.turnId]?[callId],
                          case .tools(let id, var group) = blocks[blockIndex],
                          let index = group.lastIndex(where: { $0.id == callId }) {
                    // Approval notices and agent prose can separate a call from its result.
                    group[index].attach(output: output, isError: isError, streaming: streaming)
                    blocks[blockIndex] = .tools(id: id, calls: group)
                }
            case .userMessage(let text, let attachments):
                flushCalls(); flushChildren()
                blocks.append(.user(id: id, text: text, attachments: attachments, outgoing: nil))
            case .assistantMessage(let text):
                flushCalls(); flushChildren()
                blocks.append(.assistant(id: id, text: text, streaming: streaming))
            case .reasoning(let text):
                flushCalls(); flushChildren()
                blocks.append(.reasoning(id: id, text: text, streaming: streaming))
            case .unknown:
                break
            }
        }

        for entry in model.log {
            switch entry {
            case .item(let item): add(item, streaming: false)
            case .notice(let notice):
                flushCalls(); flushChildren()
                blocks.append(.notice(notice))
            case .child(let sessionId, let task):
                flushCalls()
                children.append(ChildRef(sessionId: sessionId, task: task))
            }
        }
        for item in model.streaming { add(item, streaming: true) }
        flushCalls()
        flushChildren()
        // The turn running now, then what waits behind it; or, idle, what was sent and the wait
        // for the agent to take it.
        let streamingVisible = model.streaming.contains { $0.body.text?.isEmpty == false }
        if model.turn != nil && !streamingVisible {
            blocks.append(.working(since: model.turnStartedAt, waiting: false))
        }
        // Messages waiting behind the running turn show above the composer, not here.
        for outgoing in model.outbox where model.turn == nil || outgoing.state != .delivered {
            blocks.append(.user(id: outgoing.id.uuidString, text: outgoing.text, outgoing: outgoing))
        }
        if model.turn == nil && model.outbox.contains(where: { $0.state == .delivered }) {
            blocks.append(.working(since: nil, waiting: true))
        }
        return blocks
    }

    /// Messages the machine has taken that wait for the running turn to end.
    static func queued(_ model: SessionModel) -> [Outgoing] {
        guard model.turn != nil else { return [] }
        return model.outbox.filter { $0.state == .delivered }
    }

    static func toolCall(id: ItemId, name: String, input: Json, running: Bool) -> ToolCall {
        let object = (try? JSONSerialization.jsonObject(with: Data(input.utf8))) as? [String: Any] ?? [:]
        let keys = ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt"]
        let summary = keys.lazy.compactMap { object[$0] as? String }.first.map(firstLine) ?? ""
        var call = ToolCall(id: id, name: name, kind: kind(name), summary: summary,
                            outcome: running ? .running : .unknown)
        if let new = object["new_string"] as? String {
            call.added = lineCount(new)
            call.removed = lineCount(object["old_string"] as? String ?? "")
        } else if let content = object["content"] as? String {
            call.added = lineCount(content)
        }
        return call
    }

    static func kind(_ name: String) -> ToolCall.Kind {
        let name = name.lowercased()
        if ["bash", "shell", "exec", "command", "terminal"].contains(where: name.contains) { return .command }
        if ["edit", "write", "patch"].contains(where: name.contains) { return .edit }
        if ["read", "view", "cat"].contains(where: name.contains) { return .read }
        if ["grep", "glob", "search", "find", "ls"].contains(where: name.contains) { return .search }
        if ["web", "fetch", "url"].contains(where: name.contains) { return .web }
        return .other
    }

    private static func lineCount(_ text: String) -> Int {
        text.isEmpty ? 0 : text.split(separator: "\n", omittingEmptySubsequences: false).count
    }
}

extension ToolCall {
    mutating func attach(output text: String, isError: Bool, streaming: Bool) {
        let lines = text.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        output = lines.first ?? ""
        moreLines = max(lines.count - 1, 0)
        outcome = streaming ? .running : isError ? .failed : .ok
    }
}
