import Foundation
import Herder

/// A session on a machine. `hostId` is the machine the app is connected to, which for a vault
/// is the vault, not the host the session runs on.
struct SessionKey: Hashable, Sendable {
    let hostId: HostId
    let sessionId: SessionId
}

/// An approval or a question waiting on someone, oldest first in its session.
struct Pending: Hashable {
    enum Kind: Hashable {
        case approval(summary: String)
        case question(text: String, choices: [String])
    }

    /// The approval or question id.
    let id: String
    let turnId: TurnId
    let kind: Kind
    var routedTo: Route
    var reason: EscalationReason?
    var note: String?
    /// When it was asked, or escalated to the user.
    var since: Date
}

/// A session's state, folded from its events as the TUI does (crates/herder-tui/src/session.rs):
/// status comes from `session_status_changed`, approvals and questions are pending until
/// resolved or answered, and a turn's questions go when the turn ends.
struct SessionModel {
    let key: SessionKey
    /// Whether the first update, with every cached event, has arrived.
    var loaded = false
    var repo: String?
    var worktree: String?
    var branch: String?
    var provider: Provider?
    var model: String?
    var accountId: AccountId?
    var mode: PermissionMode?
    var parent: SessionId?
    var task: String?
    var status: SessionStatus = .idle
    var turn: TurnId?
    var approvals: [Pending] = []
    var questions: [Pending] = []
    var prs: [PullRequest] = []
    /// Why the last turn failed, until the next one starts.
    var failure: String?
    var lastMessage: String?
    /// The last tool call of the turn running now.
    var lastTool: String?
    var streaming: [Item] = []
    var updatedAt: Date?
    /// The transcript: completed items, events worth a line, and spawned children, in order.
    var log: [LogEntry] = []
    /// What the user sent from this device, until the session takes it as a user message.
    var outbox: [Outgoing] = []
    /// When the running turn started.
    var turnStartedAt: Date?
    /// Every event, worded, oldest first.
    var timeline: [TimelineEntry] = []
    var stats = SessionStats()

    init(key: SessionKey) {
        self.key = key
    }

    mutating func apply(_ update: SessionUpdate) {
        loaded = true
        for event in update.events { apply(event) }
        streaming = update.streaming
    }

    mutating func apply(_ event: Event) {
        record(event)
        let at = Timestamp.date(event.at) ?? updatedAt ?? .now
        count(event, at: at)
        timeline.append(TimelineEntry(seq: event.seq, at: at, text: Self.describe(event.body), by: event.by))
        updatedAt = at
        switch event.body {
        case .sessionCreated(let repo, let worktree, let branch, let provider, let accountId, let model, let mode, let parent, let task, _, _):
            self.repo = repo
            self.worktree = worktree
            self.branch = branch
            self.provider = provider
            self.accountId = accountId
            self.model = model
            self.mode = mode
            self.parent = parent
            self.task = task
        case .branchCheckedOut(let branch):
            self.branch = branch
        case .sessionStatusChanged(let status, _):
            self.status = status
        case .turnStarted(let turnId):
            turn = turnId
            turnStartedAt = at
            lastTool = nil
            failure = nil
        case .turnCompleted(let turnId), .turnInterrupted(let turnId):
            endTurn(turnId)
        case .turnFailed(let turnId, let error):
            endTurn(turnId)
            failure = error.message
        case .itemAdded(let item) where item.parentCallId == nil:
            switch item.body {
            case .assistantMessage(let text): lastMessage = text
            case .toolCall(let name, let input): lastTool = toolSummary(name: name, input: input)
            default: break
            }
        case .approvalRequested(let id, let turnId, _, let summary, let routedTo, let reason):
            approvals.append(Pending(
                id: id, turnId: turnId, kind: .approval(summary: summary), routedTo: routedTo,
                reason: reason, since: at))
        case .approvalEscalated(let id, let reason, let note):
            escalate(&approvals, id: id, reason: reason, note: note, at: at)
        case .approvalResolved(let id, _, _):
            approvals.removeAll { $0.id == id }
        case .questionAsked(let id, let turnId, let text, let choices, let routedTo, let reason):
            questions.append(Pending(
                id: id, turnId: turnId, kind: .question(text: text, choices: choices),
                routedTo: routedTo, reason: reason, since: at))
        case .questionEscalated(let id, let reason, let note):
            escalate(&questions, id: id, reason: reason, note: note, at: at)
        case .questionAnswered(let id, _, _):
            questions.removeAll { $0.id == id }
        case .modelSwitched(let model):
            self.model = model
        case .accountSwitched(let accountId):
            self.accountId = accountId
        case .providerSwitched(let provider, let accountId, let model):
            self.provider = provider
            self.accountId = accountId
            self.model = model
        case .permissionModeChanged(let mode):
            self.mode = mode
        case .prLinked(let pr), .prUpdated(let pr):
            if let index = prs.firstIndex(where: { $0.number == pr.number }) {
                prs[index] = pr
            } else {
                prs.append(pr)
            }
        case .prUnlinked(let number):
            prs.removeAll { $0.number == number }
        case .childSpawned, .childReported, .titleChanged, .unknown:
            break
        }
    }

    /// Adds an event to the transcript, worded as the TUI words it
    /// (crates/herder-tui/src/views/transcript.rs); before `apply` changes the state, so an
    /// answer can name the choice it picked.
    private mutating func record(_ event: Event) {
        func notice(_ text: String, _ tone: Notice.Tone = .info) {
            log.append(.notice(Notice(id: event.seq, text: text, tone: tone)))
        }
        func asked(_ what: String, _ text: String, _ routedTo: Route, _ reason: EscalationReason?) {
            switch (routedTo, reason) {
            case (.primary, _): notice("\(what) for the primary session: \(text)")
            case (.user, let reason?): notice("\(what) for you (\(reason.text.lowercased())): \(text)", .attention)
            case (.user, nil): notice("\(what): \(text)", .attention)
            }
        }
        func escalated(_ what: String, _ reason: EscalationReason, _ note: String?) {
            notice("\(what) escalated to you: \(reason.text.lowercased())" + (note.map { "; the primary says: \($0)" } ?? ""),
                   .attention)
        }
        switch event.body {
        case .itemAdded(let item):
            log.append(.item(item))
            if item.parentCallId == nil, case .userMessage(let text, _) = item.body, let index = outbox.firstIndex(where: { $0.text == text }) {
                outbox.remove(at: index)
            }
        case .branchCheckedOut(let branch): notice("Checked out \(branch)")
        case .turnInterrupted: notice("Turn interrupted")
        case .turnFailed(_, let error): notice("Turn failed: \(error.message)", .error)
        case .approvalRequested(_, _, _, let summary, let routedTo, let reason):
            asked("Approval", summary, routedTo, reason)
        case .questionAsked(_, _, let text, _, let routedTo, let reason):
            asked("Question", text, routedTo, reason)
        case .approvalEscalated(_, let reason, let note): escalated("Approval", reason, note)
        case .questionEscalated(_, let reason, let note): escalated("Question", reason, note)
        case .approvalResolved(_, let decision, let answeredBy):
            let what = switch decision {
            case .allow: "Allowed"
            case .deny: "Denied"
            case .expired: "Approval expired"
            }
            notice(answeredBy == .user ? what : "\(what) by the primary session")
        case .questionAnswered(let id, let answer, let answeredBy):
            let text: String = switch answer {
            case .text(let text): text
            case .choice(let index):
                questions.first { $0.id == id }.flatMap { pending -> String? in
                    guard case .question(_, let choices) = pending.kind, Int(index) < choices.count else { return nil }
                    return choices[Int(index)]
                } ?? "choice \(index + 1)"
            }
            notice(answeredBy == .user ? "Answered: \(text)" : "The primary session answered: \(text)")
        case .childSpawned(let child, let task): log.append(.child(sessionId: child, task: task))
        case .childReported(_, _, let summary): notice("Child reported: \(summary)")
        case .modelSwitched(let model): notice("Model switched to \(model)")
        case .accountSwitched(let accountId):
            notice(event.by == nil
                   ? "Failed over to account \(accountId): the last one hit its limit"
                   : "Account switched to \(accountId)")
        case .providerSwitched(let provider, let accountId, let model):
            notice(event.by == nil
                   ? "Failed over to \(provider) (\(model), account \(accountId)): the last one hit its limit"
                   : "Switched to \(provider) (\(model), account \(accountId))")
        case .permissionModeChanged(let mode): notice("Permission mode set to \(mode.label.lowercased())")
        case .prLinked(let pr): notice("Pull request #\(pr.number) linked: \(pr.title)")
        case .prUnlinked(let number): notice("Pull request #\(number) unlinked")
        case .sessionCreated, .sessionStatusChanged, .turnStarted, .turnCompleted, .prUpdated,
             .titleChanged, .unknown:
            break
        }
    }

    /// Keeps the session's statistics.
    private mutating func count(_ event: Event, at: Date) {
        switch event.body {
        case .sessionCreated: stats.createdAt = at
        case .turnStarted: stats.turns += 1
        case .turnCompleted(let turnId), .turnInterrupted(let turnId), .turnFailed(let turnId, _):
            if turn == turnId, let started = turnStartedAt { stats.busy += at.timeIntervalSince(started) }
            if case .turnCompleted = event.body { stats.completed += 1 }
            if case .turnInterrupted = event.body { stats.interrupted += 1 }
            if case .turnFailed = event.body { stats.failed += 1 }
        case .itemAdded(let item) where item.parentCallId == nil:
            switch item.body {
            case .userMessage: stats.prompts += 1
            case .assistantMessage: stats.replies += 1
            case .toolCall(let name, _): stats.tools[name, default: 0] += 1
            default: break
            }
        case .approvalRequested: stats.approvals += 1
        case .approvalResolved(_, let decision, _):
            if decision == .allow { stats.allowed += 1 } else { stats.denied += 1 }
        case .questionAsked: stats.questions += 1
        case .modelSwitched, .accountSwitched, .providerSwitched: stats.switches += 1
        default: break
        }
    }

    /// An event as one line of the session's history.
    static func describe(_ body: EventBody) -> String {
        switch body {
        case .sessionCreated(_, _, let branch, let provider, let accountId, let model, _, _, _, _, _):
            return "Created on \(branch) · \(provider) \(model) · \(accountId)"
        case .branchCheckedOut(let branch): return "Checked out \(branch)"
        case .sessionStatusChanged(let status, _): return "Status: \(status)"
        case .turnStarted: return "Turn started"
        case .turnCompleted: return "Turn completed"
        case .turnInterrupted: return "Turn interrupted"
        case .turnFailed(_, let error): return "Turn failed: \(firstLine(error.message))"
        case .itemAdded(let item) where item.parentCallId == nil:
            switch item.body {
            case .userMessage(let text, _): return "Prompt: \(firstLine(text))"
            case .assistantMessage(let text): return "Reply: \(firstLine(text))"
            case .reasoning: return "Thinking"
            case .toolCall(let name, let input): return "Tool: \(toolSummary(name: name, input: input))"
            case .toolResult(_, _, let isError): return isError ? "Tool failed" : "Tool finished"
            case .unknown: return "Item"
            }
        case .approvalRequested(_, _, _, let summary, _, _): return "Approval asked: \(summary)"
        case .approvalEscalated: return "Approval escalated to you"
        case .approvalResolved(_, let decision, _): return "Approval \(decision)"
        case .questionAsked(_, _, let text, _, _, _): return "Question: \(firstLine(text))"
        case .questionEscalated: return "Question escalated to you"
        case .questionAnswered: return "Question answered"
        case .childSpawned(_, let task): return "Spawned child: \(task)"
        case .childReported(_, _, let summary): return "Child reported: \(firstLine(summary))"
        case .modelSwitched(let model): return "Model: \(model)"
        case .accountSwitched(let accountId): return "Account: \(accountId)"
        case .providerSwitched(let provider, let accountId, let model): return "Provider: \(provider) \(model) · \(accountId)"
        case .permissionModeChanged(let mode): return "Permissions: \(mode.label)"
        case .prLinked(let pr): return "PR #\(pr.number) linked"
        case .prUpdated(let pr): return "PR #\(pr.number) \(pr.state.word.lowercased()), CI \(pr.ci)"
        case .prUnlinked(let number): return "PR #\(number) unlinked"
        case .titleChanged(let title, _): return "Title: \(title)"
        case .unknown: return "Event"
        }
    }

    private mutating func endTurn(_ turnId: TurnId) {
        if turn == turnId { turn = nil }
        questions.removeAll { $0.turnId == turnId }
    }

    private func escalate(
        _ list: inout [Pending], id: String, reason: EscalationReason, note: String?, at: Date
    ) {
        guard let index = list.firstIndex(where: { $0.id == id }) else { return }
        list[index].routedTo = .user
        list[index].reason = reason
        list[index].note = note
        list[index].since = at
    }

    /// Requests the user must answer; the ones routed to a primary session are its to answer.
    var forUser: [Pending] {
        (approvals + questions).filter { $0.routedTo == .user }
    }

    var needsUser: Bool {
        status == .needsYou || !forUser.isEmpty
    }

    var state: SessionState {
        if !forUser.isEmpty { return .needsYou }
        switch status {
        case .running: return .running
        case .waitingForCapacity: return .waiting
        case .needsYou: return .needsYou
        case .error: return .error
        case .archived: return .archived
        case .moved: return .moved
        case .idle, .unknown: return .idle
        }
    }

    /// The task label, else the branch, as the TUI names a session in a project; `nil` until
    /// the session's creation is known.
    var title: String? {
        task ?? branch
    }

    /// What the session is doing now, or the last thing it said.
    var activity: String {
        if let request = forUser.first {
            switch request.kind {
            case .approval(let summary): return summary
            case .question(let text, _): return text
            }
        }
        switch state {
        case .running:
            if let item = streaming.last(where: { $0.parentCallId == nil }), let text = item.body.text, !text.isEmpty {
                return firstLine(text)
            }
            return lastTool ?? "Working…"
        case .waiting: return "Waiting for a free slot"
        case .error: return failure.map { "Turn failed: \(firstLine($0))" } ?? "Error"
        case .archived: return "Archived"
        case .moved: return "Moved to another host"
        case .needsYou: return "Needs you"
        case .idle: return lastMessage.map(firstLine) ?? (loaded ? "Idle" : "")
        }
    }
}

extension ItemBody {
    /// The text of a message, reasoning or tool output.
    var text: String? {
        switch self {
        case .userMessage(let text, _), .assistantMessage(let text), .reasoning(let text): text
        case .toolResult(_, let output, _): output
        case .toolCall, .unknown: nil
        }
    }
}

/// A tool call as one line: its name and the first telling string of its input, as the TUI
/// shows it (crates/herder-tui/src/views/transcript.rs).
func toolSummary(name: String, input: Json) -> String {
    let keys = ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt"]
    let object = (try? JSONSerialization.jsonObject(with: Data(input.utf8))) as? [String: Any]
    if let value = keys.lazy.compactMap({ object?[$0] as? String }).first {
        return "\(name) \(firstLine(value))"
    }
    return name
}

func firstLine(_ text: String) -> String {
    text.split(whereSeparator: \.isNewline).first.map(String.init)?
        .trimmingCharacters(in: .whitespaces) ?? ""
}

enum Timestamp {
    nonisolated(unsafe) private static let fractional: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()
    nonisolated(unsafe) private static let whole = ISO8601DateFormatter()

    /// Parses an RFC 3339 timestamp, with or without fractional seconds.
    static func date(_ text: String) -> Date? {
        fractional.date(from: text) ?? whole.date(from: text)
    }

    /// How long ago, compactly: "now", "5m", "2h", "3d".
    static func age(_ date: Date?, now: Date) -> String {
        guard let date else { return "" }
        let seconds = Int(now.timeIntervalSince(date))
        switch seconds {
        case ..<60: return "now"
        case ..<3600: return "\(seconds / 60)m"
        case ..<86400: return "\(seconds / 3600)h"
        default: return "\(seconds / 86400)d"
        }
    }

    /// How long until, as the TUI writes it: "5d 3h", "2h 13m", "40m".
    static func until(_ date: Date?, now: Date) -> String {
        guard let date else { return "" }
        let minutes = max(0, Int(date.timeIntervalSince(now)) / 60)
        let (days, hours) = (minutes / 1440, minutes % 1440 / 60)
        if days > 0 { return "\(days)d \(hours)h" }
        if hours > 0 { return "\(hours)h \(minutes % 60)m" }
        return "\(minutes)m"
    }
}

/// One line of a session's transcript.
enum LogEntry: Hashable {
    case item(Item)
    case notice(Notice)
    case child(sessionId: SessionId, task: String)
}

/// An event shown as a line of the transcript.
struct Notice: Hashable {
    enum Tone: Hashable { case info, attention, error }
    let id: UInt64
    let text: String
    let tone: Tone
}

extension PermissionMode {
    var label: String {
        switch self {
        case .readOnly: "Read only"
        case .ask: "Ask"
        case .autoEdit: "Auto edit"
        case .fullAccess: "Full access"
        }
    }
}

/// A prompt sent from this device, shown until the session takes it.
struct Outgoing: Hashable, Identifiable {
    enum State: Hashable {
        case sending
        /// The daemon has it; it runs when the session is free.
        case delivered
        case failed(String)
    }

    let id = UUID()
    let text: String
    /// The images sent with it, shown until the session takes it.
    var images: [Data] = []
    var state: State = .sending
}

/// One event of a session's history.
struct TimelineEntry: Hashable, Identifiable {
    var id: UInt64 { seq }
    let seq: UInt64
    let at: Date
    let text: String
    /// Who caused it, when someone did.
    let by: UserId?
}

/// Counts over a session's life.
struct SessionStats: Hashable {
    var createdAt: Date?
    var turns = 0
    var completed = 0
    var failed = 0
    var interrupted = 0
    /// Time spent in turns.
    var busy: TimeInterval = 0
    var prompts = 0
    var replies = 0
    var tools: [String: Int] = [:]
    var approvals = 0
    var allowed = 0
    var denied = 0
    var questions = 0
    var switches = 0
}
