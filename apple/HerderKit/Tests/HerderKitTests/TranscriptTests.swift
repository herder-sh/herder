import Foundation
import Herder
@testable import HerderKit
import Testing

private func item(_ id: String, _ body: ItemBody, turn: String = "t1") -> EventBody {
    .itemAdded(item: Item(id: id, turnId: turn, body: body))
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
            .turnCompleted(turnId: "t1"),
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
        #expect(notices == ["Question: Which?", "Answered: Blue", "Model switched to haiku", "Turn failed: boom"])
    }

    @Test func aSentPromptShowsWhereItIsUntilTheSessionTakesIt() {
        var script = Script()
        var model = script.model([created(), .turnStarted(turnId: "t1")])
        let outgoing = Outgoing(text: "next", state: .delivered)
        model.outbox = [outgoing]
        // Running: the turn's progress; the message waits in the tray above the composer.
        #expect(Transcript.blocks(model).last == .working(since: model.turnStartedAt, waiting: false))
        #expect(Transcript.queued(model) == [outgoing])
        // Idle: the message, then the wait for the agent to take it.
        model.apply(script.event(.turnCompleted(turnId: "t1")))
        #expect(Transcript.blocks(model).last == .working(since: nil, waiting: true))
        model.apply(script.event(item("u2", .userMessage(text: "next", attachments: []), turn: "t2")))
        #expect(model.outbox.isEmpty)
    }

    @Test func claudeSessionsStartOnOpus() {
        #expect(ModelCatalog.defaultModel("claude") == "claude-opus-5-5")
        #expect(ModelCatalog.name("claude-opus-5-5", provider: "claude") == "Claude Opus 5.5")
        #expect(ModelCatalog.defaultModel("codex") == "")
    }

    @Test func streamingItemsFollowTheLog() {
        var script = Script()
        var model = script.model([created(), .turnStarted(turnId: "t1")])
        model.apply(SessionUpdate(events: [], streaming: [Item(id: "s", turnId: "t1", body: .assistantMessage(text: "Hel"))]))
        #expect(Transcript.blocks(model).last == .assistant(id: "t1/s", text: "Hel", streaming: true))
    }
}

@MainActor
struct DefaultAccountTests {
    private func account(_ id: String, _ provider: String, used: Double) -> Account {
        Account(accountId: id, provider: provider, label: id, usage: [UsageWindow(window: "five_hour", usedPercent: used, resetsAt: nil)])
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

    @Test func repliesOfDifferentTurnsKeepApartWhenTheAdapterReusesItemIds() {
        var script = Script()
        let model = script.model([
            created(),
            .turnStarted(turnId: "t1"),
            item("u1", .userMessage(text: "asd", attachments: []), turn: "t1"),
            item("item-2", .assistantMessage(text: "First."), turn: "t1"),
            .turnCompleted(turnId: "t1"),
            .turnStarted(turnId: "t2"),
            item("u2", .userMessage(text: "asd", attachments: []), turn: "t2"),
            item("item-2", .assistantMessage(text: "Second."), turn: "t2"),
            .turnCompleted(turnId: "t2"),
        ])
        let blocks = Transcript.blocks(model)
        #expect(Set(blocks.map(\.id)).count == blocks.count)
        #expect(blocks.compactMap { if case .assistant(_, let text, _) = $0 { text } else { nil } } == ["First.", "Second."])
    }

    @Test func messagesQueuedBehindTheTurnLeaveTheTranscriptForTheTray() {
        var script = Script()
        var model = script.model([created(), .turnStarted(turnId: "t1")])
        model.outbox = [Outgoing(text: "next", images: [], state: .delivered), Outgoing(text: "typing", images: [], state: .sending)]
        let users = Transcript.blocks(model).compactMap { if case .user(_, let text, _, _) = $0 { text } else { nil } }
        #expect(users == ["typing"])
        #expect(Transcript.queued(model).map(\.text) == ["next"])
    }
}
