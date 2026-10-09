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
    /// A user message; `outgoing` while it is on its way from this device. `followUp` when herder
    /// sent it on its own.
    case user(id: String, text: String, attachments: [Attachment] = [], outgoing: Outgoing?, agentMessage: AgentMessage? = nil,
              followUp: FollowUp? = nil)
    /// The agent is working, or about to, since the date.
    case working(since: Date?, waiting: Bool)
    case assistant(id: String, text: String, streaming: Bool)
    case reasoning(id: String, text: String, streaming: Bool)
    case tools(id: String, calls: [ToolCall])
    /// A page the agent showed with `show_html`, in its own card.
    case visual(HtmlVisual)
    case children(id: String, [ChildRef])
    case report(ChildReport)
    case agents(id: String, [NativeAgent])
    case notice(Notice)
    case question(AskedQuestion)
    case handoff(Handoff)
    /// A finished turn's tool calls, reasoning and passing prose, behind one line.
    case work(TurnWork)

    var id: String {
        switch self {
        case .user(let id, _, _, _, _, _), .assistant(let id, _, _), .reasoning(let id, _, _), .tools(let id, _),
             .children(let id, _): id
        case .agents(let id, _): id
        case .visual(let visual): "visual-\(visual.id)"
        case .working: "working"
        case .notice(let notice): "notice-\(notice.id)"
        case .question(let question): "question-\(question.seq)"
        case .handoff(let handoff): "handoff-\(handoff.id)"
        case .report(let report): "report-\(report.id)"
        case .work(let work): work.id
        }
    }

}

/// What a finished turn did on the way to its answer, in order.
struct TurnWork: Hashable, Identifiable {
    let turnId: TurnId
    /// From the turn's start to its end; `nil` when the journal lacks either.
    let duration: TimeInterval?
    let blocks: [TranscriptBlock]

    var id: String { "work-\(turnId)" }

    /// "Worked for 1m 5s", or "Worked" without a duration.
    var title: String {
        guard let duration else { return "Worked" }
        let seconds = Int(duration.rounded())
        let (hours, minutes) = (seconds / 3600, seconds % 3600 / 60)
        if hours > 0 { return "Worked for \(hours)h \(minutes)m" }
        if minutes > 0 { return "Worked for \(minutes)m \(seconds % 60)s" }
        return "Worked for \(seconds)s"
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
    static func blocks(_ model: SessionModel) -> [TranscriptBlock] { blocks(model, parent: nil) }

    static func blocks(_ model: SessionModel, parent: NativeAgent.ID?) -> [TranscriptBlock] {
        build(model, parent: parent).blocks
    }

    /// The session's transcript with each finished turn's work folded behind a `.work` block:
    /// what stays in view is the user's messages, each turn's last reply, and what needs the user.
    static func collapsed(_ model: SessionModel) -> [TranscriptBlock] {
        let (blocks, turns) = build(model, parent: nil)
        var hidden: [TurnId: [Int]] = [:]
        var replies: [TurnId: Int] = [:]
        for (index, block) in blocks.enumerated() {
            guard let turn = turns[index], turn != model.turn else { continue }
            switch block {
            case .assistant:
                if let reply = replies[turn] { hidden[turn, default: []].append(reply) }
                replies[turn] = index
            case .tools, .reasoning, .agents:
                hidden[turn, default: []].append(index)
            default:
                break
            }
        }
        // The work sits above the turn's last reply, or where it starts when it follows that reply.
        var folds: [Int: TurnWork] = [:]
        for (turn, indices) in hidden {
            let indices = indices.sorted()
            guard let first = indices.first else { continue }
            let reply = replies[turn].flatMap { $0 > first ? $0 : nil }
            folds[reply ?? first] = TurnWork(turnId: turn, duration: model.turnTimes[turn]?.duration,
                                             blocks: indices.map { blocks[$0] })
        }
        let folded = Set(hidden.values.joined())
        var result: [TranscriptBlock] = []
        for (index, block) in blocks.enumerated() {
            if let work = folds[index] { result.append(.work(work)) }
            if !folded.contains(index) { result.append(block) }
        }
        return result
    }

    /// The blocks, each with the turn of the items it shows, if any.
    private static func build(_ model: SessionModel, parent: NativeAgent.ID?) -> (blocks: [TranscriptBlock], turns: [TurnId?]) {
        var blocks: [TranscriptBlock] = []
        var turns: [TurnId?] = []
        func append(_ block: TranscriptBlock, turn: TurnId? = nil) {
            blocks.append(block)
            turns.append(turn)
        }
        var calls: [ToolCall] = []
        var callsTurn = ""
        var completedGroups: [TurnId: [ItemId: Int]] = [:]
        var children: [ChildRef] = []
        var agents: [NativeAgent] = []

        func flushCalls() {
            if let first = calls.first {
                for call in calls { completedGroups[callsTurn, default: [:]][call.id] = blocks.count }
                append(.tools(id: "tools-\(callsTurn)/\(first.id)", calls: calls), turn: callsTurn)
            }
            calls = []
        }
        func flushChildren() {
            if let first = children.first { append(.children(id: "children-\(first.sessionId)", children)) }
            children = []
        }
        func flushAgents() {
            if let first = agents.first {
                append(.agents(id: "agents-\(first.id.turnId)-\(first.id.callId)", agents), turn: first.id.turnId)
            }
            agents = []
        }
        let items = model.log.compactMap { entry -> Item? in
            if case .item(let item) = entry { item } else { nil }
        } + model.streaming
        func belongs(_ item: Item) -> Bool {
            if let parent { return item.turnId == parent.turnId && item.parentCallId == parent.callId }
            return item.parentCallId == nil
        }
        func add(_ item: Item, streaming: Bool) {
            guard belongs(item) else { return }
            // Adapters number items per turn, so an item id alone repeats across turns.
            let id = "\(item.turnId)/\(item.id)"
            switch item.body {
            case .toolCall(let name, let input) where HtmlVisual.isShowHtml(name):
                flushCalls(); flushChildren(); flushAgents()
                append(.visual(HtmlVisual(id: id, input: input, streaming: streaming)), turn: item.turnId)
            case .toolCall(let name, let input):
                flushChildren()
                if NativeAgent.isAgent(name) || items.contains(where: {
                    $0.turnId == item.turnId && $0.parentCallId == item.id
                }) {
                    flushCalls()
                    agents.append(NativeAgent(item: item, items: items, runningTurn: model.turn,
                                              working: model.status == .running, streaming: model.streaming,
                                              times: model.itemTimes))
                } else {
                    flushAgents()
                    if callsTurn != item.turnId { flushCalls() }
                    if calls.isEmpty { callsTurn = item.turnId }
                    var call = toolCall(id: item.id, name: name, input: input, running: streaming || model.turn == item.turnId)
                    // Results can arrive after intervening prose or another agent's output.
                    if let result = items.last(where: { result in
                        guard result.turnId == item.turnId, result.parentCallId == item.parentCallId,
                              case .toolResult(let callId, _, _) = result.body else { return false }
                        return callId == item.id
                    }), case .toolResult(_, let output, let isError) = result.body {
                        call.attach(output: output, isError: isError, streaming: model.streaming.contains(result))
                    }
                    calls.append(call)
                }
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
                flushCalls(); flushChildren(); flushAgents()
                append(.user(id: id, text: text, attachments: attachments, outgoing: nil, agentMessage: item.agentMessage,
                             followUp: item.followUp), turn: item.turnId)
            case .assistantMessage(let text):
                flushCalls(); flushChildren(); flushAgents()
                append(.assistant(id: id, text: text, streaming: streaming), turn: item.turnId)
            case .reasoning(let text):
                flushCalls(); flushChildren(); flushAgents()
                append(.reasoning(id: id, text: text, streaming: streaming), turn: item.turnId)
            case .unknown:
                break
            }
        }

        for entry in model.log {
            switch entry {
            case .item(let item): add(item, streaming: false)
            case .notice(let notice):
                guard parent == nil else { continue }
                flushCalls(); flushChildren(); flushAgents()
                append(.notice(notice))
            case .question(let question):
                guard parent == nil else { continue }
                flushCalls(); flushChildren(); flushAgents()
                append(.question(question))
            case .handoff(let handoff):
                guard parent == nil else { continue }
                flushCalls(); flushChildren(); flushAgents()
                append(.handoff(handoff))
            case .report(let report):
                guard parent == nil else { continue }
                flushCalls(); flushChildren(); flushAgents()
                append(.report(report))
            case .child(let sessionId, let task):
                guard parent == nil else { continue }
                flushCalls(); flushAgents()
                children.append(ChildRef(sessionId: sessionId, task: task))
            }
        }
        for item in model.streaming { add(item, streaming: true) }
        flushCalls()
        flushChildren()
        flushAgents()
        if parent != nil { return (blocks, turns) }
        // The turn running now, then what waits behind it; or, idle, what was sent and the wait
        // for the agent to take it.
        let streamingVisible = model.streaming.contains { $0.parentCallId == nil && $0.body.text?.isEmpty == false }
        if model.turn != nil && !streamingVisible {
            append(.working(since: model.turnStartedAt, waiting: false))
        }
        // What the machine queued shows above the composer, from its queue, not here.
        for outgoing in model.outbox {
            append(.user(id: outgoing.id.uuidString, text: outgoing.text, outgoing: outgoing))
        }
        if model.turn == nil && model.outbox.contains(where: { $0.state == .delivered }) {
            append(.working(since: nil, waiting: true))
        }
        return (blocks, turns)
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

    /// A run of tool calls in a line, by kind in the order they first ran: "Ran 4 commands,
    /// edited 2 files". Edits and reads count the files they touched.
    static func summary(_ calls: [ToolCall]) -> String {
        var kinds: [ToolCall.Kind] = []
        for call in calls where !kinds.contains(call.kind) { kinds.append(call.kind) }
        let parts = kinds.map { kind -> String in
            let of = calls.filter { $0.kind == kind }
            let files = Set(of.map(\.summary)).count
            func counted(_ count: Int, _ one: String, _ many: String) -> String { "\(count) \(count == 1 ? one : many)" }
            return switch kind {
            case .command: "ran " + counted(of.count, "command", "commands")
            case .edit: "edited " + counted(files, "file", "files")
            case .read: "read " + counted(files, "file", "files")
            case .search: "searched " + counted(of.count, "time", "times")
            case .web: "fetched " + counted(of.count, "page", "pages")
            case .other: "used " + counted(of.count, "tool", "tools")
            }
        }
        let line = parts.joined(separator: ", ")
        return line.prefix(1).uppercased() + line.dropFirst()
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
