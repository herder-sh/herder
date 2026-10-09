import Foundation
import Herder
@testable import HerderKit
import Testing

private func item(_ id: String, _ body: ItemBody, turn: String = "t1") -> EventBody {
    .itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: id, turnId: turn, body: body))
}

struct TranscriptTests {
    @Test func toolCallsGroupWithTheirResults() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            item("u", .userMessage(text: "Fix it", attachments: [])),
            item("c1", .toolCall(name: "Bash", input: #"{"command":"cargo test"}"#)),
            item("r1", .toolResult(callId: "c1", output: "running 3 tests\nok\n", isError: false)),
            item("c2", .toolCall(name: "Edit", input: #"{"file_path":"a.rs","old_string":"x","new_string":"y\nz"}"#)),
            item("r2", .toolResult(callId: "c2", output: "no such file", isError: true)),
            item("a", .assistantMessage(text: "Done.")),
            .turnCompleted(turnId: "t1", usage: nil),
        ])
        let blocks = Transcript.blocks(model)
        guard blocks.count == 3, case .tools(_, let calls) = blocks[1] else {
            Issue.record("unexpected blocks: \(blocks)")
            return
        }
        #expect(calls.map(\.summary) == ["cargo test", "a.rs"])
        #expect(calls.map(\.outcome) == [.ok, .failed])
        #expect(calls[0].output == "running 3 tests")
        #expect(calls[0].moreLines == 1)
        #expect((calls[1].added, calls[1].removed) == (2, 1))
        #expect(calls[1].kind == .edit)
    }

    @Test func toolGroupsStaySeparateAcrossTurnsWithReusedIds() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            item("c1", .toolCall(name: "Bash", input: "{}")),
            item("r1", .toolResult(callId: "c1", output: "first", isError: false)),
            .turnCompleted(turnId: "t1", usage: nil), .turnStarted(turnId: "t2"),
            item("c1", .toolCall(name: "Bash", input: "{}"), turn: "t2"),
            item("r1", .toolResult(callId: "c1", output: "second", isError: false), turn: "t2"),
            .turnCompleted(turnId: "t2", usage: nil),
        ])
        let groups = Transcript.blocks(model).compactMap { block -> [ToolCall]? in
            if case .tools(_, let calls) = block { return calls }
            return nil
        }
        #expect(groups.count == 2)
        #expect(groups.map { $0.map(\.output) } == [["first"], ["second"]])
    }

    @Test func resultsAttachAcrossApprovalNoticesAndProseDuringStreamingAndReplay() {
        var script = Script()
        var model = script.model([
            created(), .turnStarted(turnId: "t1"),
            item("c1", .toolCall(name: "Bash", input: "{}")),
            .approvalRequested(approvalId: "a1", turnId: "t1", toolCallId: "c1", summary: "Run command?", routedTo: .user, reason: nil),
            .approvalResolved(approvalId: "a1", decision: .allow, answeredBy: .user),
            item("a", .assistantMessage(text: "Waiting for the command.")),
        ])
        let result = Item(agentMessage: nil, parentCallId: nil, id: "r1", turnId: "t1", body: .toolResult(callId: "c1", output: "partial", isError: false))
        model.apply(SessionUpdate(events: [], streaming: [result]))
        guard case .tools(_, let streamingCalls) = Transcript.blocks(model)[0] else {
            Issue.record("missing tool group")
            return
        }
        #expect(streamingCalls[0].output == "partial")
        #expect(streamingCalls[0].outcome == .running)
        model.apply(SessionUpdate(events: [
            script.event(item("r1", .toolResult(callId: "c1", output: "failed", isError: true))),
            script.event(.turnCompleted(turnId: "t1", usage: nil)),
            script.event(.turnStarted(turnId: "t2")),
            script.event(item("c1", .toolCall(name: "Bash", input: "{}"), turn: "t2")),
            script.event(item("r1", .toolResult(callId: "c1", output: "second", isError: false), turn: "t2")),
            script.event(.turnCompleted(turnId: "t2", usage: nil)),
        ], streaming: []))
        let groups = Transcript.blocks(model).compactMap { block -> [ToolCall]? in
            if case .tools(_, let calls) = block { return calls }
            return nil
        }
        #expect(groups.map { $0.map(\.output) } == [["failed"], ["second"]])
        #expect(groups.map { $0.map(\.outcome) } == [[.failed], [.ok]])
    }

    @Test func eventsBecomeNoticesWordedAsInTheTUI() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            .questionAsked(questionId: "q", turnId: "t1", text: "Which?", choices: ["Red", "Blue"], routedTo: .user, reason: nil),
            .questionAnswered(questionId: "q", answer: .choice(index: 1), answeredBy: .user),
            .modelSwitched(model: "haiku"),
            .turnFailed(turnId: "t1", error: TurnError(class: .fatal, message: "boom")),
        ])
        let notices = Transcript.blocks(model).compactMap { block -> String? in
            if case .notice(let notice) = block { notice.text } else { nil }
        }
        #expect(notices == ["Turn failed: boom"])
    }

    @Test func aQuestionIsOneBlockItsAnswerFillsIn() {
        var script = Script()
        let asked: [EventBody] = [
            created(), .turnStarted(turnId: "t1"),
            .questionAsked(questionId: "q", turnId: "t1", text: "Which?\n\n- **Red**: warm", choices: ["Red", "Blue"],
                           routedTo: .user, reason: nil),
        ]
        let waiting = Transcript.blocks(script.model(asked)).compactMap { block -> AskedQuestion? in
            if case .question(let question) = block { question } else { nil }
        }
        #expect(waiting.map(\.answer) == [nil])

        script = Script()
        let answered = Transcript.blocks(script.model(asked + [
            .questionAnswered(questionId: "q", answer: .choice(index: 1), answeredBy: .user),
        ])).compactMap { block -> AskedQuestion? in
            if case .question(let question) = block { question } else { nil }
        }
        #expect(answered.map(\.picked) == [1])
        #expect(answered.map(\.answeredBy) == [.user])
    }

    @Test func switchesBecomeHandoffsFromWhatTheSessionRanOn() {
        var script = Script()
        var model = script.model([created()])
        model.apply(SessionUpdate(events: [
            script.event(.modelSwitched(model: "haiku"), by: "tomas"),
            script.event(.accountSwitched(accountId: "work"), by: "tomas"),
            // The daemon's own switch is a handoff like any other.
            script.event(.providerSwitched(provider: "codex", accountId: "gpt", model: "gpt-6.1-sol")),
        ], streaming: []))
        let handoffs = Transcript.blocks(model).compactMap { block -> Handoff? in
            if case .handoff(let handoff) = block { handoff } else { nil }
        }
        let claude = { (model: String, account: AccountId) in Handoff.Side(provider: "claude", model: model, accountId: account) }
        #expect(handoffs.map(\.kind) == [.model, .account, .provider])
        #expect(handoffs.map(\.from) == [claude("opus", "main"), claude("haiku", "main"), claude("haiku", "work")])
        #expect(handoffs.map(\.to) == [claude("haiku", "main"), claude("haiku", "work"), Handoff.Side(provider: "codex", model: "gpt-6.1-sol", accountId: "gpt")])
    }

    @Test func theProviderDefaultResolvingToAModelIsNoHandoff() {
        var script = Script()
        var model = script.model([created(model: "")])
        model.apply(SessionUpdate(events: [
            // The CLI reports the model "" stood for: no handoff, just the model it runs.
            script.event(.modelSwitched(model: "claude-opus-5-5")),
            script.event(.modelSwitched(model: "claude-sonnet-5-5"), by: "tomas"),
        ], streaming: []))
        let handoffs = Transcript.blocks(model).compactMap { block -> Handoff? in
            if case .handoff(let handoff) = block { handoff } else { nil }
        }
        #expect(model.model == "claude-sonnet-5-5")
        #expect(handoffs.map(\.kind) == [.model])
        #expect(handoffs.map(\.from.model) == ["claude-opus-5-5"])
        #expect(handoffs.map(\.to.model) == ["claude-sonnet-5-5"])
    }

    @Test func aForkIsAHandoffBetweenMachines() {
        var script = Script("01B", host: "host-b")
        var model = script.model([created()])
        model.apply(SessionUpdate(events: [
            script.event(.sessionForked(fromSession: "01A", fromHost: "host-a"), by: "tomas"),
            script.event(.accountSwitched(accountId: "work"), by: "tomas"),
        ], streaming: []))
        let handoffs = Transcript.blocks(model).compactMap { block -> Handoff? in
            if case .handoff(let handoff) = block { handoff } else { nil }
        }
        // The fork's move to an account of its new machine is part of the same handoff.
        #expect(handoffs.map(\.kind) == [.machine])
        #expect(handoffs[0].from == Handoff.Side(provider: "claude", model: "opus", accountId: "main", hostId: "host-a"))
        #expect(handoffs[0].to == Handoff.Side(provider: "claude", model: "opus", accountId: "work", hostId: "host-b"))
        let moments = model.moments.compactMap { moment -> Handoff? in
            if case .handoff(let handoff) = moment.kind { handoff } else { nil }
        }
        #expect(moments == handoffs)
    }

    @Test func aSentPromptShowsUntilTheQueueOrTheSessionTakesIt() {
        var script = Script()
        var model = script.model([created(), .turnStarted(turnId: "t1")])
        let outgoing = Outgoing(text: "next", state: .delivered)
        model.outbox = [outgoing]
        // Running: the message shows until the machine's queue lists it; the tray has it then.
        #expect(Transcript.blocks(model).last == .user(id: outgoing.id.uuidString, text: "next", outgoing: outgoing))
        model.settle([QueuedPrompt(promptId: "p1", text: "next", images: 0, by: "sample", agentMessage: nil)])
        #expect(model.outbox.isEmpty)
        #expect(Transcript.blocks(model).last == .working(since: model.turnStartedAt, waiting: false))
        // Idle: the message, then the wait for the agent to take it, until it does.
        model.apply(script.event(.turnCompleted(turnId: "t1", usage: nil)))
        model.outbox = [outgoing]
        #expect(Transcript.blocks(model).last == .working(since: nil, waiting: true))
        model.apply(script.event(item("u2", .userMessage(text: "next", attachments: []), turn: "t2")))
        #expect(model.outbox.isEmpty)
    }

    @Test func aPromptAboutToStartStaysInTheTranscriptNotTheTray() {
        var script = Script()
        var model = script.model([created()])
        let outgoing = Outgoing(text: "first", state: .delivered)
        model.outbox = [outgoing]
        // No turn runs: the machine lists the prompt only until it takes it to start the CLI.
        let queue = [QueuedPrompt(promptId: "p1", text: "first", images: 0, by: "sample", agentMessage: nil)]
        model.settle(queue)
        #expect(model.outbox == [outgoing])
        #expect(model.waiting(in: queue).isEmpty)
        model.settle([])
        #expect(Transcript.blocks(model).contains(.user(id: outgoing.id.uuidString, text: "first", outgoing: outgoing)))
    }

    @Test func claudeSessionsStartOnOpus() {
        #expect(ModelCatalog.defaultModel("claude") == "claude-opus-5-5")
        #expect(ModelCatalog.name("claude-opus-5-5", provider: "claude") == "Claude Opus 5.5")
        #expect(ModelCatalog.defaultModel("codex") == "")
    }

    @Test func streamingItemsFollowTheLog() {
        var script = Script()
        var model = script.model([created(), .turnStarted(turnId: "t1")])
        model.apply(SessionUpdate(events: [], streaming: [Item(agentMessage: nil, parentCallId: nil, id: "s", turnId: "t1", body: .assistantMessage(text: "Hel"))]))
        #expect(Transcript.blocks(model).last == .assistant(id: "t1/s", text: "Hel", streaming: true))
    }
}

@MainActor
struct DefaultAccountTests {
    private func account(_ id: String, _ provider: String, used: Double, fallback: Bool = false) -> Account {
        Account(accountId: id, provider: provider, label: id, configDir: nil, email: nil, usage: [UsageWindow(window: "five_hour", usedPercent: used, resetsAt: nil)], fallback: fallback)
    }

    @Test func theProjectsAccountWinsElseTheLeastUsed() throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        var host = machine("h", name: "h", sessions: [],
                           projects: [Project(projectId: "p", name: "p", paths: [], defaultPermissionMode: nil, defaultAccount: "busy", setupCommand: nil)])
        host.accounts = [account("busy", "claude", used: 90), account("idle", "claude", used: 10), account("gpt", "codex", used: 0)]
        fleet.setMachinesForTesting([host])
        #expect(fleet.defaultAccount(on: "h", projectId: "p", provider: "claude")?.accountId == "busy")
        #expect(fleet.defaultAccount(on: "h", projectId: nil, provider: "claude")?.accountId == "idle")
        #expect(fleet.defaultAccount(on: "h", projectId: "p", provider: "codex")?.accountId == "gpt")
    }

    @Test func aFallbackAccountIsPickedOnlyOnceEveryOtherIsUsedUp() throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        var host = machine("h", name: "h", sessions: [],
                           projects: [Project(projectId: "p", name: "p", paths: [], defaultPermissionMode: nil, defaultAccount: "spare", setupCommand: nil)])
        host.accounts = [account("main", "claude", used: 90), account("spare", "claude", used: 0, fallback: true)]
        fleet.setMachinesForTesting([host])
        #expect(fleet.defaultAccount(on: "h", projectId: nil, provider: "claude")?.accountId == "main")
        // A project naming it still uses it.
        #expect(fleet.defaultAccount(on: "h", projectId: "p", provider: "claude")?.accountId == "spare")
        host.accounts[0] = account("main", "claude", used: 100)
        fleet.setMachinesForTesting([host])
        #expect(fleet.defaultAccount(on: "h", projectId: nil, provider: "claude")?.accountId == "spare")
    }

    @Test func repliesOfDifferentTurnsKeepApartWhenTheAdapterReusesItemIds() {
        var script = Script()
        let model = script.model([
            created(),
            .turnStarted(turnId: "t1"),
            item("u1", .userMessage(text: "asd", attachments: []), turn: "t1"),
            item("item-2", .assistantMessage(text: "First."), turn: "t1"),
            .turnCompleted(turnId: "t1", usage: nil),
            .turnStarted(turnId: "t2"),
            item("u2", .userMessage(text: "asd", attachments: []), turn: "t2"),
            item("item-2", .assistantMessage(text: "Second."), turn: "t2"),
            .turnCompleted(turnId: "t2", usage: nil),
        ])
        let blocks = Transcript.blocks(model)
        #expect(Set(blocks.map(\.id)).count == blocks.count)
        #expect(blocks.compactMap { if case .assistant(_, let text, _) = $0 { text } else { nil } } == ["First.", "Second."])
    }

    @Test func onlyTheQueuedCopyOfADeliveredMessageSettlesIt() {
        var model = SessionModel(key: SessionKey(hostId: "h", sessionId: "s"))
        model.turn = "t1"
        let typing = Outgoing(text: "next", images: [], state: .sending)
        let failed = Outgoing(text: "next", images: [], state: .failed("refused"))
        let delivered = Outgoing(text: "next", images: [], state: .delivered)
        model.outbox = [typing, failed, delivered]
        let agent = AgentMessage(senderSessionId: "other", messageId: "m1", hopCount: 1, permissionCeiling: .ask)
        model.settle([QueuedPrompt(promptId: "p1", text: "next", images: 0, by: nil, agentMessage: agent)])
        #expect(model.outbox == [typing, failed, delivered])
        model.settle([QueuedPrompt(promptId: "p2", text: "next", images: 0, by: "sample", agentMessage: nil)])
        #expect(model.outbox == [typing, failed])
    }
}

struct TurnFailureTests {
    @Test func aLimitSaysSoAndWhenItResets() {
        let error = TurnError(class: .limitReached, message: "You've hit your individual spend limit · run /usage-credits to raise it, or visit claude.ai/admin-settings/usage · your session limit resets 11:20pm (Europe/Vilnius)")
        #expect(TurnFailure.summary(error) == "usage limit reached · resets 11:20pm (Europe/Vilnius)")
    }

    @Test func otherErrorsKeepTheirFirstClause() {
        #expect(TurnFailure.summary(TurnError(class: .fatal, message: "boom")) == "boom")
        #expect(TurnFailure.summary(TurnError(class: .transient, message: "overloaded · try again\nlater")) == "overloaded")
    }

    @Test func aShortenedFailureKeepsTheWholeMessage() {
        var script = Script()
        let message = "spend limit · your session limit resets 9pm"
        let model = script.model([created(), .turnStarted(turnId: "t1"), .turnFailed(turnId: "t1", error: TurnError(class: .limitReached, message: message))])
        let notice = Transcript.blocks(model).compactMap { block -> Notice? in
            if case .notice(let notice) = block { notice } else { nil }
        }.last
        #expect(notice?.text == "Turn failed: usage limit reached · resets 9pm")
        #expect(notice?.detail == message)
    }
}
