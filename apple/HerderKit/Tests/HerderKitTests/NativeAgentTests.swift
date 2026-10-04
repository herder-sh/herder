import Foundation
import Herder
@testable import HerderKit
import Testing

private func nestedItem(_ id: String, _ body: ItemBody, turn: String = "t1", parent: String? = nil) -> EventBody {
    .itemAdded(item: Item(agentMessage: nil, parentCallId: parent, id: id, turnId: turn, body: body))
}

private let launchMetadata = """
    Async agent launched successfully. (This tool result is internal metadata, never quote it.)
    agentId: a1b2c3 (internal ID)
    The agent is working in the background. You will be notified automatically when it completes.
    output_file: /tmp/claude/tasks/a1b2c3.output
    """

private let agentInput = #"{"description":"Review accounts","prompt":"Check owner permissions","subagent_type":"Explore"}"#

struct NativeAgentTests {
    @Test func parallelAgentsGroupAndOpenOnlyTheirOwnMessages() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            nestedItem("a", .toolCall(name: "Agent", input: agentInput)),
            nestedItem("b", .toolCall(name: "Task", input: #"{"description":"Review colors"}"#)),
            nestedItem("a-text", .assistantMessage(text: "Checking permissions"), parent: "a"),
            nestedItem("b-text", .assistantMessage(text: "Checking contrast"), parent: "b"),
            nestedItem("parent-text", .assistantMessage(text: "Both reviews started")),
            nestedItem("a-result", .toolResult(callId: "a", output: "Permissions pass", isError: false)),
        ])
        let blocks = Transcript.blocks(model)
        guard case .agents(_, let agents) = blocks.first else {
            Issue.record("Expected a grouped sub-chat card"); return
        }
        #expect(agents.count == 2)
        #expect(agents[0].title == "Review accounts")
        #expect(agents[0].prompt == "Check owner permissions")
        #expect(agents[0].outcome == .ok)
        #expect(agents[0].result == "Permissions pass")
        #expect(agents[1].outcome == .running)
        #expect(Transcript.blocks(model, parent: agents[0].id) == [
            .assistant(id: "t1/a-text", text: "Checking permissions", streaming: false),
        ])
        #expect(Transcript.blocks(model, parent: agents[1].id) == [
            .assistant(id: "t1/b-text", text: "Checking contrast", streaming: false),
        ])
        #expect(model.lastMessage == "Both reviews started")
        #expect(!blocks.contains(.assistant(id: "t1/a-text", text: "Checking permissions", streaming: false)))
    }

    @Test func nestedToolResultsSurviveInterleavedProseAndGrandchildren() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            nestedItem("a", .toolCall(name: "Agent", input: agentInput)),
            nestedItem("read", .toolCall(name: "Read", input: #"{"file_path":"accounts.rs"}"#), parent: "a"),
            nestedItem("thinking", .assistantMessage(text: "Reading the file"), parent: "a"),
            nestedItem("read-result", .toolResult(callId: "read", output: "owner guard", isError: false), parent: "a"),
            nestedItem("grandchild", .toolCall(name: "Agent", input: agentInput), parent: "a"),
            nestedItem("deep", .assistantMessage(text: "Deep review"), parent: "grandchild"),
        ])
        let blocks = Transcript.blocks(model, parent: .init(turnId: "t1", callId: "a"))
        guard case .tools(_, let calls) = blocks.first, case .agents(_, let nested) = blocks.last else {
            Issue.record("Expected tool and grandchild cards"); return
        }
        #expect(calls[0].outcome == .ok)
        #expect(calls[0].output == "owner guard")
        #expect(Transcript.blocks(model, parent: nested[0].id) == [
            .assistant(id: "t1/deep", text: "Deep review", streaming: false),
        ])
    }

    @Test func reusedCallIdsRemainScopedToTheirTurn() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            nestedItem("a", .toolCall(name: "Agent", input: agentInput)),
            nestedItem("old", .assistantMessage(text: "Old work"), parent: "a"),
            nestedItem("result", .toolResult(callId: "a", output: "Failed", isError: true)),
            .turnCompleted(turnId: "t1"), .turnStarted(turnId: "t2"),
            nestedItem("a", .toolCall(name: "Agent", input: agentInput), turn: "t2"),
            nestedItem("new", .assistantMessage(text: "New work"), turn: "t2", parent: "a"),
        ])
        let old = NativeAgent.find(.init(turnId: "t1", callId: "a"), in: model)
        let new = NativeAgent.find(.init(turnId: "t2", callId: "a"), in: model)
        #expect(old?.outcome == .failed)
        #expect(new?.outcome == .running)
        #expect(old?.id != new?.id)
        #expect(Transcript.blocks(model, parent: .init(turnId: "t2", callId: "a")) == [
            .assistant(id: "t2/new", text: "New work", streaming: false),
        ])
    }

    @Test func oldRecordingsWithoutChildMessagesStillShowTaskAndFullResult() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            nestedItem("a", .toolCall(name: "Task", input: agentInput)),
            nestedItem("result", .toolResult(callId: "a", output: "First line\nSecond line", isError: false)),
            .turnCompleted(turnId: "t1"),
        ])
        let agent = NativeAgent.find(.init(turnId: "t1", callId: "a"), in: model)
        #expect(agent?.result == "First line\nSecond line")
        #expect(agent?.outcome == .ok)
        #expect(Transcript.blocks(model, parent: .init(turnId: "t1", callId: "a")).isEmpty)
    }

    @Test func streamingResultStaysWorkingUntilItIsJournaled() {
        var script = Script()
        var model = script.model([
            created(), .turnStarted(turnId: "t1"),
            nestedItem("a", .toolCall(name: "Agent", input: agentInput)),
        ])
        let result = Item(agentMessage: nil, parentCallId: nil, id: "result", turnId: "t1",
                          body: .toolResult(callId: "a", output: "Partial", isError: false))
        model.streaming = [result]
        let reference = NativeAgent.ID(turnId: "t1", callId: "a")
        #expect(NativeAgent.find(reference, in: model)?.outcome == .running)
        model.streaming = []
        model.apply(script.event(.itemAdded(item: result)))
        #expect(NativeAgent.find(reference, in: model)?.outcome == .ok)
    }

    @Test func backgroundLaunchAcknowledgementIsNotCompletion() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            nestedItem("a", .toolCall(name: "Agent", input: #"{"description":"Background review","run_in_background":true}"#)),
            nestedItem("launch", .toolResult(callId: "a", output: "Agent launched", isError: false)),
            .turnCompleted(turnId: "t1"),
        ])
        let agent = NativeAgent.find(.init(turnId: "t1", callId: "a"), in: model)
        #expect(agent?.outcome == .unknown)
        #expect(agent?.launched == true)
        #expect(agent?.result == nil)
        #expect(agent?.status == "In the background")
        #expect(NativeAgent.summary(agent.map { [$0] } ?? []) == "1 background")
    }

    @Test func backgroundLaunchMetadataIsNeverTheResult() {
        // Claude runs some agents in the background without the call asking for it; only the
        // acknowledgement's shape tells.
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            nestedItem("a", .toolCall(name: "Agent", input: agentInput)),
            nestedItem("launch", .toolResult(callId: "a", output: launchMetadata, isError: false)),
        ])
        let reference = NativeAgent.ID(turnId: "t1", callId: "a")
        let running = NativeAgent.find(reference, in: model)
        #expect(running?.background == true)
        #expect(running?.result == nil)
        #expect(running?.outcome == .running)
        #expect(running?.status == "Running in the background")
        #expect(running?.duration(at: .distantFuture) != nil)

        var ended = model
        ended.apply(script.event(.turnCompleted(turnId: "t1")))
        let after = NativeAgent.find(reference, in: ended)
        #expect(after?.result == nil)
        #expect(after?.outcome == .unknown)
        #expect(after?.badge.text == "Background")
        #expect(after?.duration(at: .distantFuture) == nil)
    }

    @Test func backgroundAgentRunsWhileTheSessionStaysRunningPastItsTurn() {
        // The daemon keeps the session running while background agents work.
        var script = Script()
        let model = script.model([
            created(), .sessionStatusChanged(status: .running, retryAt: nil), .turnStarted(turnId: "t1"),
            nestedItem("a", .toolCall(name: "Agent", input: agentInput)),
            nestedItem("launch", .toolResult(callId: "a", output: launchMetadata, isError: false)),
            .turnCompleted(turnId: "t1"),
            nestedItem("a-grep", .toolCall(name: "Grep", input: #"{"pattern":"TODO"}"#), parent: "a"),
        ])
        let reference = NativeAgent.ID(turnId: "t1", callId: "a")
        let running = NativeAgent.find(reference, in: model)
        #expect(running?.outcome == .running)
        #expect(running?.status == "Running in the background")
        #expect(NativeAgent.summary(running.map { [$0] } ?? []) == "1 working")

        var stopped = model
        stopped.apply(script.event(.sessionStatusChanged(status: .idle, retryAt: nil)))
        #expect(NativeAgent.find(reference, in: stopped)?.outcome == .unknown)
    }

    @Test func backgroundAgentResultFromALaterTurnCompletesIt() {
        // Claude reports a background agent's result in a turn it starts itself, as a later
        // result for the launching call.
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            nestedItem("a", .toolCall(name: "Agent", input: agentInput)),
            nestedItem("launch", .toolResult(callId: "a", output: launchMetadata, isError: false)),
            .turnCompleted(turnId: "t1"), .turnStarted(turnId: "cli"),
            nestedItem("done", .toolResult(callId: "a", output: "Found 2 issues", isError: false), turn: "cli"),
            nestedItem("reply", .assistantMessage(text: "The review found 2 issues"), turn: "cli"),
            .turnCompleted(turnId: "cli"),
        ])
        let agent = NativeAgent.find(.init(turnId: "t1", callId: "a"), in: model)
        #expect(agent?.launched == true)
        #expect(agent?.result == "Found 2 issues")
        #expect(agent?.outcome == .ok)
        #expect(agent?.status == "Completed")
        #expect(NativeAgent.summary(agent.map { [$0] } ?? []) == "1 completed")
    }

    @Test func failedBackgroundLaunchShowsItsError() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            nestedItem("a", .toolCall(name: "Agent", input: #"{"description":"Review","run_in_background":true}"#)),
            nestedItem("launch", .toolResult(callId: "a", output: "Unknown agent type", isError: true)),
            .turnCompleted(turnId: "t1"),
        ])
        let agent = NativeAgent.find(.init(turnId: "t1", callId: "a"), in: model)
        #expect(agent?.launched == false)
        #expect(agent?.outcome == .failed)
        #expect(agent?.result == "Unknown agent type")
    }

    @Test func syncAgentKeepsItsResultKindModelAndRunTime() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            nestedItem("a", .toolCall(name: "Agent", input: #"{"description":"Count files","prompt":"Count","subagent_type":"Explore","model":"haiku"}"#)),
            nestedItem("a-text", .assistantMessage(text: "Counting"), parent: "a"),
            nestedItem("result", .toolResult(callId: "a", output: "**42** files", isError: false)),
            .turnCompleted(turnId: "t1"),
        ])
        let agent = NativeAgent.find(.init(turnId: "t1", callId: "a"), in: model)
        #expect(agent?.background == false)
        #expect(agent?.result == "**42** files")
        #expect(agent?.outcome == .ok)
        #expect(agent?.kind == "Explore")
        #expect(agent?.model == "haiku")
        #expect(agent?.title == "Count files")
        // The script spaces events a second apart: the call is the 3rd, its result the 5th.
        #expect(agent?.duration(at: .distantFuture) == 2)
    }

    @Test func interruptedAgentIsNotReportedAsCompleted() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            nestedItem("a", .toolCall(name: "Agent", input: agentInput)),
            .turnInterrupted(turnId: "t1"),
        ])
        let agent = NativeAgent.find(.init(turnId: "t1", callId: "a"), in: model)
        #expect(agent?.outcome == .unknown)
        #expect(agent?.status == "Stopped without a result")
        #expect(NativeAgent.summary(agent.map { [$0] } ?? []) == "1 stopped")
    }

    @Test func listsShowRunningAndJustFinishedAgentsUnderTheirSessionWhileItWorks() {
        var script = Script("01A")
        var model = script.model([
            created(task: "Review"), .sessionStatusChanged(status: .running, retryAt: nil),
            .turnStarted(turnId: "t0"),
            nestedItem("old", .toolCall(name: "Agent", input: #"{"description":"Earlier review"}"#), turn: "t0"),
            nestedItem("old-result", .toolResult(callId: "old", output: "Done before", isError: false), turn: "t0"),
            .turnCompleted(turnId: "t0"), .turnStarted(turnId: "t1"),
            nestedItem("a", .toolCall(name: "Agent", input: agentInput), turn: "t1"),
            nestedItem("b", .toolCall(name: "Task", input: #"{"description":"Review colors"}"#), turn: "t1"),
            nestedItem("c", .toolCall(name: "Agent", input: #"{"description":"Background scan"}"#), turn: "t1"),
            nestedItem("read", .toolCall(name: "Read", input: #"{"file_path":"a.rs"}"#), turn: "t1"),
            nestedItem("nested", .toolCall(name: "Agent", input: #"{"description":"Nested"}"#), turn: "t1", parent: "a"),
            nestedItem("a-result", .toolResult(callId: "a", output: "Permissions pass", isError: false), turn: "t1"),
            nestedItem("c-launch", .toolResult(callId: "c", output: launchMetadata, isError: false), turn: "t1"),
        ])
        let lists = Lists(machines: [machine("host-a", name: "a", sessions: ["01A"])], sessions: [script.key: model])
        let agents = lists.home.first?.agents ?? []
        #expect(agents.map(\.title) == ["Review accounts", "Review colors", "Background scan"])
        #expect(agents.map(\.outcome) == [.ok, .running, .running])
        #expect(agents.map(\.badge.text) == ["Completed", "Running", "Background"])
        #expect(lists.projects.first?.live.first?.agents == agents)

        // The turn ends; the session keeps running for the background agent alone.
        model.apply(script.event(nestedItem("b-result", .toolResult(callId: "b", output: "Contrast fine", isError: false))))
        model.apply(script.event(.turnCompleted(turnId: "t1")))
        #expect(NativeAgent.listed(in: model).map(\.title) == ["Review accounts", "Review colors", "Background scan"])

        model.apply(script.event(.sessionStatusChanged(status: .idle, retryAt: nil)))
        #expect(NativeAgent.listed(in: model).isEmpty)
    }
}
