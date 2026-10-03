import Foundation
import Herder
@testable import HerderKit
import Testing

private func nestedItem(_ id: String, _ body: ItemBody, turn: String = "t1", parent: String? = nil) -> EventBody {
    .itemAdded(item: Item(parentCallId: parent, id: id, turnId: turn, body: body))
}

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
            .assistant(id: "a-text", text: "Checking permissions", streaming: false),
        ])
        #expect(Transcript.blocks(model, parent: agents[1].id) == [
            .assistant(id: "b-text", text: "Checking contrast", streaming: false),
        ])
        #expect(model.lastMessage == "Both reviews started")
        #expect(!blocks.contains(.assistant(id: "a-text", text: "Checking permissions", streaming: false)))
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
            .assistant(id: "deep", text: "Deep review", streaming: false),
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
            .assistant(id: "new", text: "New work", streaming: false),
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
    }
}
