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
    /// The title a user or the small model gave the session, from `title_changed`.
    var titled: String?
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
    /// What the user sent from this device, until the session takes it as a user message or
    /// the machine's queue lists it.
    var outbox: [Outgoing] = []
    /// When the running turn started.
    var turnStartedAt: Date?
    /// Every event, worded, oldest first.
    var timeline: [TimelineEntry] = []
    /// The events that matter, oldest first, with a turn's tool calls folded into one.
    var moments: [Moment] = []
    /// Where each turn's folded tool calls sit in `moments`.
    private var toolMoments: [TurnId: Int] = [:]
    var stats = SessionStats()
    /// How each ended turn ended, for the reports a child sends its parent.
    var turnEnds: [TurnId: TurnEnd] = [:]
    /// When each item was journaled, by "turn/item": how long a provider's own sub-agent ran.
    var itemTimes: [String: Date] = [:]

    init(key: SessionKey) {
        self.key = key
    }

    /// Hands what this device sent over to the machine's queue once it lists it: the queue
    /// tray shows it from then on.
    mutating func settle(_ queue: [QueuedPrompt]) {
        for prompt in queue where prompt.agentMessage == nil {
            if let index = outbox.firstIndex(where: { $0.state == .delivered && $0.text == prompt.text }) {
                outbox.remove(at: index)
            }
        }
    }

    mutating func apply(_ update: SessionUpdate) {
        loaded = true
        for event in update.events { apply(event) }
        streaming = update.streaming
    }

    mutating func apply(_ event: Event) {
        record(event)
        let at = Timestamp.date(event.at) ?? updatedAt ?? .now
        note(event, at: at)
        count(event, at: at)
        timeline.append(TimelineEntry(seq: event.seq, at: at, text: Self.describe(event.body), by: event.by))
        updatedAt = at
        if case .itemAdded(let item) = event.body { itemTimes["\(item.turnId)/\(item.id)"] = at }
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
        case .turnCompleted(let turnId):
            endTurn(turnId)
            turnEnds[turnId] = .completed
        case .turnInterrupted(let turnId):
            endTurn(turnId)
            turnEnds[turnId] = .interrupted
        case .turnFailed(let turnId, let error):
            endTurn(turnId)
            turnEnds[turnId] = .failed
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
        case .titleChanged(let title, _):
            titled = title
        case .itemAdded, .childSpawned, .childReported, .sessionForked, .unknown:
            break
        }
    }

    /// Adds an event to the transcript, worded as the TUI words it
    /// (crates/herder-tui/src/views/transcript.rs); before `apply` changes the state, so an
    /// answer can name the choice it picked.
    private mutating func record(_ event: Event) {
        func notice(_ text: String, _ tone: Notice.Tone = .info, detail: String? = nil) {
            log.append(.notice(Notice(id: event.seq, text: text, tone: tone, detail: detail)))
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
            if item.parentCallId == nil, item.agentMessage == nil, case .userMessage(let text, _) = item.body, let index = outbox.firstIndex(where: { $0.text == text }) {
                outbox.remove(at: index)
            }
        case .branchCheckedOut(let branch): notice("Checked out \(branch)")
        case .turnInterrupted: notice("Turn interrupted")
        case .turnFailed(_, let error):
            let summary = TurnFailure.summary(error)
            notice("Turn failed: \(summary)", .error, detail: summary == error.message ? nil : error.message)
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
            let text = answerText(id, answer)
            notice(answeredBy == .user ? "Answered: \(text)" : "The primary session answered: \(text)")
        case .childSpawned(let child, let task): log.append(.child(sessionId: child, task: task))
        case .childReported(let child, let turnId, let summary):
            log.append(.report(ChildReport(id: event.seq, sessionId: child, turnId: turnId, summary: summary)))
        case .modelSwitched, .accountSwitched, .providerSwitched, .sessionForked:
            if let handoff = handoff(event) { log.append(.handoff(handoff)) }
        case .permissionModeChanged(let mode): notice("Permission mode set to \(mode.label.lowercased())")
        case .prLinked(let pr): notice("Pull request #\(pr.number) linked: \(pr.title)")
        case .prUnlinked(let number): notice("Pull request #\(number) unlinked")
        case .sessionCreated, .sessionStatusChanged, .turnStarted, .turnCompleted, .prUpdated,
             .titleChanged, .unknown:
            break
        }
    }

    /// What a switch or a fork moved the session from and to; before `apply` changes the state.
    private func handoff(_ event: Event) -> Handoff? {
        let side: (Handoff.Kind, Handoff.Side)
        var from: HostId?
        switch event.body {
        case .modelSwitched(let model):
            // From the provider's default to the model the CLI reports is the model resolving,
            // not a handoff.
            guard self.model?.isEmpty == false else { return nil }
            side = (.model, Handoff.Side(provider: provider, model: model, accountId: accountId))
        case .accountSwitched(let accountId):
            side = (.account, Handoff.Side(provider: provider, model: model, accountId: accountId))
        case .providerSwitched(let provider, let accountId, let model):
            side = (.provider, Handoff.Side(provider: provider, model: model, accountId: accountId))
        case .sessionForked(_, let fromHost):
            from = fromHost
            side = (.machine, Handoff.Side(provider: provider, model: model, accountId: accountId, hostId: key.hostId))
        default: return nil
        }
        return Handoff(id: event.seq, kind: side.0,
                       from: Handoff.Side(provider: provider, model: model, accountId: accountId, hostId: from), to: side.1)
    }

    /// An answer in words: the choice it picked, while the question is still pending.
    private func answerText(_ id: QuestionId, _ answer: Answer) -> String {
        switch answer {
        case .text(let text): text
        case .choice(let index):
            questions.first { $0.id == id }.flatMap { pending -> String? in
                guard case .question(_, let choices) = pending.kind, Int(index) < choices.count else { return nil }
                return choices[Int(index)]
            } ?? "choice \(index + 1)"
        }
    }

    /// Adds the events that matter to `moments`, before `apply` changes the state: a turn's tool
    /// calls fold into one moment, and status changes, replies and tool results are left out.
    private mutating func note(_ event: Event, at: Date) {
        func add(_ kind: Moment.Kind, turn: TurnId? = self.turn) {
            moments.append(Moment(id: event.seq, at: at, turn: turn, kind: kind))
        }
        switch event.body {
        case .sessionCreated(_, _, let branch, _, _, _, _, _, _, _, _): add(.created(branch: branch))
        case .branchCheckedOut(let branch): add(.setting("Checked out \(branch)"))
        case .permissionModeChanged(let mode): add(.setting("Permissions set to \(mode.label.lowercased())"))
        case .turnCompleted(let turnId), .turnInterrupted(let turnId), .turnFailed(let turnId, _):
            let duration = turn == turnId ? turnStartedAt.map { at.timeIntervalSince($0) } : nil
            let kind: Moment.Kind = switch event.body {
            case .turnFailed(_, let error): .turnEnded(.failed, duration: duration, error: TurnFailure.summary(error))
            case .turnInterrupted: .turnEnded(.interrupted, duration: duration, error: nil)
            default: .turnEnded(.completed, duration: duration, error: nil)
            }
            add(kind, turn: turnId)
        case .itemAdded(let item) where item.parentCallId == nil:
            switch item.body {
            case .userMessage(let text, _): add(.prompt(text, from: item.agentMessage?.senderSessionId), turn: item.turnId)
            case .toolCall(let name, _):
                if let index = toolMoments[item.turnId], case .tools(var counts) = moments[index].kind {
                    counts[name, default: 0] += 1
                    moments[index].kind = .tools(counts)
                } else {
                    toolMoments[item.turnId] = moments.count
                    add(.tools([name: 1]), turn: item.turnId)
                }
            default: break
            }
        case .approvalRequested(_, let turnId, _, let summary, _, _): add(.approval(summary), turn: turnId)
        case .approvalResolved(_, let decision, let answeredBy): add(.decided(decision, byUser: answeredBy == .user))
        case .questionAsked(_, let turnId, let text, _, _, _): add(.question(text), turn: turnId)
        case .questionAnswered(let id, let answer, let answeredBy):
            add(.answered(answerText(id, answer), byUser: answeredBy == .user))
        case .childSpawned(let child, let task): add(.spawned(child, task: task))
        case .childReported(let child, let turnId, let summary): add(.reported(child, summary: summary), turn: turnId)
        case .modelSwitched, .accountSwitched, .providerSwitched, .sessionForked:
            if let handoff = handoff(event) { add(.handoff(handoff)) }
        case .prLinked(let pr): add(.pr(pr, change: "linked"))
        case .prUpdated(let pr):
            // CI and review churn stays out; a merge, a close or a reopen is worth a line.
            if prs.first(where: { $0.number == pr.number })?.state != pr.state {
                add(.pr(pr, change: pr.state.word.lowercased()))
            }
        case .prUnlinked(let number): add(.prUnlinked(number))
        case .titleChanged(let title, _): add(.titled(title))
        case .sessionStatusChanged, .turnStarted, .itemAdded, .approvalEscalated, .questionEscalated, .unknown:
            break
        }
    }

    /// The moments in runs that share a turn, oldest first: what a turn did, between what
    /// happened outside turns.
    var momentGroups: [MomentGroup] {
        let turns = Dictionary(stats.turnLog.map { ($0.id, $0) }, uniquingKeysWith: { $1 })
        var groups: [MomentGroup] = []
        for moment in moments {
            if let last = groups.last, last.turnId == moment.turn {
                groups[groups.count - 1].moments.append(moment)
            } else {
                groups.append(MomentGroup(turnId: moment.turn, turn: moment.turn.flatMap { turns[$0] }, moments: [moment]))
            }
        }
        return groups
    }

    /// Keeps the session's statistics.
    private mutating func count(_ event: Event, at: Date) {
        switch event.body {
        case .sessionCreated: stats.createdAt = at
        case .turnStarted(let turnId):
            stats.turns += 1
            stats.turnLog.append(TurnRecord(id: turnId, number: stats.turns, started: at, provider: provider, model: model))
        case .turnCompleted(let turnId), .turnInterrupted(let turnId), .turnFailed(let turnId, _):
            if turn == turnId, let started = turnStartedAt { stats.busy += at.timeIntervalSince(started) }
            let end: TurnEnd = switch event.body {
            case .turnCompleted: .completed
            case .turnInterrupted: .interrupted
            default: .failed
            }
            switch end {
            case .completed: stats.completed += 1
            case .interrupted: stats.interrupted += 1
            case .failed: stats.failed += 1
            }
            if let index = stats.turnLog.lastIndex(where: { $0.id == turnId }), stats.turnLog[index].end == nil {
                stats.turnLog[index].ended = at
                stats.turnLog[index].end = end
            }
        case .itemAdded(let item) where item.parentCallId == nil:
            switch item.body {
            case .userMessage: stats.prompts += 1
            case .assistantMessage: stats.replies += 1
            case .toolCall(let name, _): stats.tools[name, default: 0] += 1
            default: break
            }
        case .approvalRequested: stats.approvals += 1
        case .approvalResolved(_, let decision, _):
            switch decision {
            case .allow: stats.allowed += 1
            case .deny: stats.denied += 1
            case .expired: stats.expired += 1
            }
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
            case .userMessage(let text, _):
                if let sender = item.agentMessage?.senderSessionId {
                    return "From agent \(sender): \(firstLine(text))"
                }
                return "Prompt: \(firstLine(text))"
            case .assistantMessage(let text): return "Reply: \(firstLine(text))"
            case .reasoning: return "Thinking"
            case .toolCall(let name, let input): return "Tool: \(toolSummary(name: name, input: input))"
            case .toolResult(_, _, let isError): return isError ? "Tool failed" : "Tool finished"
            case .unknown: return "Item"
            }
        case .itemAdded: return "Sub-agent activity"
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
        case .sessionForked(let fromSession, let fromHost): return "Machine: from \(fromHost) (\(fromSession))"
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

    /// The session's title, else its task label, else its branch, as the TUI names a session
    /// in a project; `nil` until the session's creation is known.
    var title: String? {
        titled ?? task ?? branch
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
        case .idle, .done: return lastMessage.map(firstLine) ?? (loaded ? "Idle" : "")
        }
    }

    /// Where the session stands as a child of another: an idle child is done once a turn
    /// completed, and failed while its last turn's failure stands.
    var progress: ChildProgress {
        switch state {
        case .running: .running
        case .waiting: .waiting
        case .needsYou: .needsYou
        case .error: .failed
        case .archived: .archived
        case .moved: .moved
        case .idle, .done: failure != nil ? .failed : stats.completed > 0 ? .done : .idle
        }
    }

    /// Time spent in turns, the running one included.
    func runTime(at now: Date) -> TimeInterval {
        stats.busy + (turn != nil ? turnStartedAt.map { max(0, now.timeIntervalSince($0)) } ?? 0 : 0)
    }
}

/// How a turn ended.
enum TurnEnd: Hashable { case completed, interrupted, failed }

/// A child session's progress, as its parent shows it.
enum ChildProgress: Hashable {
    case running, waiting, needsYou, idle, done, failed, archived, moved

    var label: String {
        switch self {
        case .running: "Running"
        case .waiting: "Waiting for a free slot"
        case .needsYou: "Needs you"
        case .idle: "Idle"
        case .done: "Done"
        case .failed: "Failed"
        case .archived: "Archived"
        case .moved: "Moved"
        }
    }

    /// The status mark that stands for it.
    var state: SessionState {
        switch self {
        case .running: .running
        case .waiting: .waiting
        case .needsYou: .needsYou
        case .idle, .done: .idle
        case .failed: .error
        case .archived: .archived
        case .moved: .moved
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
    case handoff(Handoff)
    case child(sessionId: SessionId, task: String)
    case report(ChildReport)
}

/// A child session ended a turn and reported back to this one.
struct ChildReport: Hashable {
    let id: UInt64
    let sessionId: SessionId
    /// The child's turn that ended.
    let turnId: TurnId
    /// The child's last message of the turn, or why it failed.
    let summary: String
}

/// An event shown as a line of the transcript.
struct Notice: Hashable {
    enum Tone: Hashable { case info, attention, error }
    let id: UInt64
    let text: String
    let tone: Tone
    /// The whole of what `text` shortens, shown on hover or a click.
    var detail: String?
}

/// A failed turn's error, in a line: what went wrong, without the provider's advice. The whole
/// message stays for the line's detail.
enum TurnFailure {
    static func summary(_ error: TurnError) -> String {
        let parts = error.message.components(separatedBy: " · ").map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
        switch error.class {
        case .limitReached:
            // "… your session limit resets 11:20pm (Europe/Vilnius)" → "resets 11:20pm (…)"
            let reset = parts.lazy.compactMap { part in
                part.range(of: "resets ", options: .caseInsensitive).map { String(part[$0.lowerBound...]) }
            }.first
            return ["usage limit reached", reset].compactMap { $0 }.joined(separator: " · ")
        case .auth:
            return "the account needs signing in again"
        case .transient, .fatal:
            let first = parts.first ?? error.message
            let sentence = first.split(separator: "\n").first.map(String.init) ?? first
            return sentence.count > 100 ? String(sentence.prefix(99)) + "…" : sentence
        }
    }
}

/// The session moved to another model, account, provider or machine: what it ran on, and what
/// it runs on from here. A fork is a move to the machine it was forked onto.
struct Handoff: Hashable {
    enum Kind: Hashable { case model, account, provider, machine }
    /// A side of the switch; `nil` where the session had not said yet.
    struct Side: Hashable {
        let provider: Provider?
        let model: String?
        let accountId: AccountId?
        /// The machine, for a move between machines: the host forked from, or the machine the
        /// app reaches the fork through.
        var hostId: HostId? = nil
    }
    let id: UInt64
    let kind: Kind
    let from: Side
    let to: Side
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
    var expired = 0
    var questions = 0
    var switches = 0
    /// Every turn, oldest first.
    var turnLog: [TurnRecord] = []

    /// The tools by use, most used first.
    var toolRanking: [(name: String, count: Int)] { Self.rank(tools) }

    /// Counts by name, highest first, then by name.
    static func rank(_ counts: [String: Int]) -> [(name: String, count: Int)] {
        counts.map { (name: $0.key, count: $0.value) }.sorted { ($0.count, $1.name) > ($1.count, $0.name) }
    }

    /// Time in ended turns by the model they ran on, most time first.
    var modelUse: [ModelUse] {
        var uses: [ModelUse] = []
        for record in turnLog {
            guard let duration = record.duration else { continue }
            if let index = uses.firstIndex(where: { $0.provider == record.provider && $0.model == record.model }) {
                uses[index].time += duration
                uses[index].turns += 1
            } else {
                uses.append(ModelUse(provider: record.provider, model: record.model, time: duration, turns: 1))
            }
        }
        return uses.sorted { $0.time > $1.time }
    }
}

/// A turn: when it ran, how it ended, and on what model.
struct TurnRecord: Hashable, Identifiable {
    let id: TurnId
    /// Its place in the session, from 1.
    let number: Int
    let started: Date
    var ended: Date?
    var end: TurnEnd?
    let provider: Provider?
    let model: String?

    var duration: TimeInterval? { ended.map { $0.timeIntervalSince(started) } }
}

/// The time a session's turns ran on one model.
struct ModelUse: Hashable {
    let provider: Provider?
    let model: String?
    var time: TimeInterval
    var turns: Int
}

/// An event worth a line in the inspector's timeline.
struct Moment: Hashable, Identifiable {
    enum Kind: Hashable {
        case created(branch: String)
        /// A prompt, from an agent when it names the session that sent it.
        case prompt(String, from: SessionId?)
        /// A turn's tool calls by tool.
        case tools([String: Int])
        case turnEnded(TurnEnd, duration: TimeInterval?, error: String?)
        case handoff(Handoff)
        case pr(PullRequest, change: String)
        case prUnlinked(UInt64)
        case approval(String)
        case decided(ApprovalOutcome, byUser: Bool)
        case question(String)
        case answered(String, byUser: Bool)
        case spawned(SessionId, task: String)
        case reported(SessionId, summary: String)
        case titled(String)
        case setting(String)
    }

    let id: UInt64
    let at: Date
    /// The turn it happened in, if any.
    let turn: TurnId?
    var kind: Kind

    /// A turn's tool calls in a line: "12 tools · Bash ×8, Edit ×3, +1 more".
    static func toolLine(_ counts: [String: Int], shown: Int = 3) -> String {
        let total = counts.values.reduce(0, +)
        let ranked = SessionStats.rank(counts)
        var names = ranked.prefix(shown).map { $0.count > 1 ? "\($0.name) ×\($0.count)" : $0.name }
        if ranked.count > shown { names.append("+\(ranked.count - shown) more") }
        return "\(total) \(total == 1 ? "tool" : "tools") · " + names.joined(separator: ", ")
    }
}

/// A run of moments in one turn, or outside any.
struct MomentGroup: Hashable, Identifiable {
    var id: UInt64 { moments.first?.id ?? 0 }
    let turnId: TurnId?
    /// The turn, when its start is known.
    let turn: TurnRecord?
    var moments: [Moment]
}
