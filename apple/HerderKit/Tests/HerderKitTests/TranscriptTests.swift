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
            item("u", .userMessage(text: "Fix it")),
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

    @Test func aQueuedPromptLeavesOnceTheSessionTakesIt() {
        var script = Script()
        var model = script.model([created(), .turnStarted(turnId: "t1")])
        model.queued = ["next"]
        #expect(Transcript.blocks(model).last == .user(id: "queued-0", text: "next", queued: true))
        model.apply(script.event(item("u2", .userMessage(text: "next"), turn: "t2")))
        #expect(model.queued.isEmpty)
    }

    @Test func streamingItemsFollowTheLog() {
        var script = Script()
        var model = script.model([created(), .turnStarted(turnId: "t1")])
        model.apply(SessionUpdate(events: [], streaming: [Item(id: "s", turnId: "t1", body: .assistantMessage(text: "Hel"))]))
        #expect(Transcript.blocks(model).last == .assistant(id: "s", text: "Hel", streaming: true))
    }
}

@MainActor
struct DefaultAccountTests {
    private func account(_ id: String, _ provider: String, used: Double) -> Account {
        Account(accountId: id, provider: provider, label: id, usage: [UsageWindow(window: "five_hour", usedPercent: used, resetsAt: nil)],
                failover: false)
    }

    @Test func theProjectsAccountWinsElseTheLeastUsed() throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        var host = machine("h", name: "h", sessions: [],
                           projects: [Project(projectId: "p", name: "p", paths: [], defaultAccount: "busy", setupCommand: nil)])
        host.accounts = [account("busy", "claude", used: 90), account("idle", "claude", used: 10), account("gpt", "codex", used: 0)]
        fleet.setMachinesForTesting([host])
        #expect(fleet.defaultAccount(on: "h", projectId: "p", provider: "claude")?.accountId == "busy")
        #expect(fleet.defaultAccount(on: "h", projectId: nil, provider: "claude")?.accountId == "idle")
        #expect(fleet.defaultAccount(on: "h", projectId: "p", provider: "codex")?.accountId == "gpt")
    }
}
