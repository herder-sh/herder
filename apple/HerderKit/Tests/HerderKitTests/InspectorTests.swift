import Foundation
import Herder
@testable import HerderKit
import Testing

private func tool(_ name: String, turn: TurnId = "t1", id: String = UUID().uuidString) -> EventBody {
    .itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: id, turnId: turn, body: .toolCall(name: name, input: "{}")))
}

private func item(_ body: ItemBody, turn: TurnId = "t1") -> EventBody {
    .itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: UUID().uuidString, turnId: turn, body: body))
}

private func pr(_ state: PrState, ci: CiStatus = .passing) -> PullRequest {
    PullRequest(number: 7, url: "", title: "Inspector", headBranch: nil, state: state, ci: ci, review: .none, mergeable: .clean)
}

struct InspectorTests {
    @Test func theInspectorIsASheetOnCompactWidthAndAPaneOtherwise() {
        #expect(InspectorPresentation(compact: true) == .sheet)
        #expect(InspectorPresentation(compact: false) == .pane)
    }

    @Test func theTimelineKeepsTheMainEventsAndFoldsATurnsTools() {
        var script = Script()
        let model = script.model([
            created(),
            .sessionStatusChanged(status: .running, retryAt: nil),
            .turnStarted(turnId: "t1"),
            item(.userMessage(text: "Fix the build", attachments: [])),
            tool("Bash"), item(.toolResult(callId: "x", output: "ok", isError: false)),
            item(.assistantMessage(text: "Looking")),
            tool("Edit"), tool("Bash"),
            .approvalRequested(approvalId: "a", turnId: "t1", toolCallId: "c", summary: "Bash: rm", routedTo: .user, reason: nil),
            .approvalResolved(approvalId: "a", decision: .deny, answeredBy: .user),
            tool("Bash"),
            .prLinked(pr: pr(.open)),
            .prUpdated(pr: pr(.open, ci: .failing)),
            .prUpdated(pr: pr(.merged)),
            .turnCompleted(turnId: "t1", usage: nil),
            .sessionStatusChanged(status: .idle, retryAt: nil),
            .titleChanged(title: "Fix the build", source: .auto),
            .modelSwitched(model: "sonnet"),
        ])
        let kinds = model.moments.map(\.kind)
        #expect(kinds == [
            .created(branch: "herder/abc"),
            .prompt("Fix the build", from: nil),
            .tools(["Bash": 3, "Edit": 1]),
            .approval("Bash: rm"),
            .decided(.deny, byUser: true),
            .pr(pr(.open), change: "linked"),
            .pr(pr(.merged), change: "merged"),
            .turnEnded(.completed, duration: 13, error: nil),
            .titled("Fix the build"),
            .handoff(Handoff(id: 19, kind: .model,
                             from: Handoff.Side(provider: "claude", model: "opus", accountId: "main"),
                             to: Handoff.Side(provider: "claude", model: "sonnet", accountId: "main"))),
        ])
        #expect(model.timeline.count == 19)
    }

    @Test func momentsGroupByTurnBetweenThoseOutsideTurns() {
        var script = Script()
        let model = script.model([
            created(),
            .turnStarted(turnId: "t1"), item(.userMessage(text: "one", attachments: [])), tool("Read"),
            .turnFailed(turnId: "t1", error: TurnError(class: .fatal, message: "boom\nstack")),
            .modelSwitched(model: "sonnet"),
            .turnStarted(turnId: "t2"), item(.userMessage(text: "two", attachments: []), turn: "t2"),
            tool("Read", turn: "t2"),
        ])
        let groups = model.momentGroups
        #expect(groups.map(\.turnId) == [nil, "t1", nil, "t2"])
        #expect(groups.map { $0.turn?.number } == [nil, 1, nil, 2])
        #expect(groups[1].moments.last?.kind == .turnEnded(.failed, duration: 3, error: "boom"))
        #expect(groups[3].moments.map(\.kind) == [.prompt("two", from: nil), .tools(["Read": 1])])
    }

    @Test func turnsAreRecordedWithTheirLengthOutcomeAndModel() {
        var script = Script()
        let model = script.model([
            created(),
            .turnStarted(turnId: "t1"), tool("Bash"), .turnCompleted(turnId: "t1", usage: nil),
            .modelSwitched(model: "sonnet"),
            .turnStarted(turnId: "t2"), tool("Bash", turn: "t2"), tool("Bash", turn: "t2"), .turnInterrupted(turnId: "t2"),
            .turnStarted(turnId: "t3"), .turnCompleted(turnId: "t3", usage: nil),
            .turnStarted(turnId: "t4"),
        ])
        let log = model.stats.turnLog
        #expect(log.map(\.number) == [1, 2, 3, 4])
        #expect(log.map(\.end) == [.completed, .interrupted, .completed, nil])
        #expect(log.map(\.duration) == [2, 3, 1, nil])
        #expect(log.map(\.model) == ["opus", "sonnet", "sonnet", "sonnet"])
        #expect(model.stats.modelUse == [
            ModelUse(provider: "claude", model: "sonnet", time: 4, turns: 2),
            ModelUse(provider: "claude", model: "opus", time: 2, turns: 1),
        ])
    }

    @Test func approvalsCountExpiredApartFromDenied() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            .approvalRequested(approvalId: "a", turnId: "t1", toolCallId: "c", summary: "x", routedTo: .user, reason: nil),
            .approvalResolved(approvalId: "a", decision: .expired, answeredBy: .user),
            .approvalRequested(approvalId: "b", turnId: "t1", toolCallId: "c", summary: "y", routedTo: .user, reason: nil),
            .approvalResolved(approvalId: "b", decision: .deny, answeredBy: .user),
        ])
        #expect((model.stats.approvals, model.stats.allowed, model.stats.denied, model.stats.expired) == (2, 0, 1, 1))
    }

    @Test func aTurnsToolsReadAsOneLine() {
        #expect(Moment.toolLine(["Bash": 1]) == "1 tool · Bash")
        #expect(Moment.toolLine(["Bash": 8, "Edit": 3, "Read": 1]) == "12 tools · Bash ×8, Edit ×3, Read")
        #expect(Moment.toolLine(["Bash": 2, "Edit": 2, "Read": 1, "Grep": 1, "Glob": 1]) == "7 tools · Bash ×2, Edit ×2, Glob, +2 more")
    }

    @Test func toolsAreColouredByWhatTheyDo() {
        #expect(ToolKind("Bash") == .shell)
        #expect(ToolKind("exec_command") == .shell)
        #expect(ToolKind("MultiEdit") == .edit)
        #expect(ToolKind("apply_patch") == .edit)
        #expect(ToolKind("Read") == .read)
        #expect(ToolKind("Grep") == .search)
        #expect(ToolKind("WebFetch") == .web)
        #expect(ToolKind("Task") == .agent)
        #expect(ToolKind("mcp__linear__save") == .other)
    }
}
