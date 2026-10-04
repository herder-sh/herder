import Foundation
import Herder
@testable import HerderKit
import Testing

struct ChildSessionTests {
    @Test func aReportIsABlockTiedToItsChildAfterTheSpawn() {
        var script = Script("01P")
        let model = script.model([
            created(task: "Lead"), .turnStarted(turnId: "t1"),
            .childSpawned(childSessionId: "01C", task: "Write tests"),
            .childSpawned(childSessionId: "01D", task: "Fix docs"),
            .itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: "a", turnId: "t1", body: .assistantMessage(text: "Waiting."))),
            .childReported(childSessionId: "01D", turnId: "c1", summary: "## Docs\n\nFixed the typos.\n- README"),
            .childReported(childSessionId: "01C", turnId: "c1", summary: "Tests pass."),
        ])
        let blocks = Transcript.blocks(model).filter { if case .working = $0 { false } else { true } }
        guard blocks.count == 4, case .children(_, let children) = blocks[0],
              case .report(let first) = blocks[2], case .report(let second) = blocks[3] else {
            Issue.record("unexpected blocks: \(blocks)")
            return
        }
        #expect(children.map(\.sessionId) == ["01C", "01D"])
        #expect((first.sessionId, first.turnId, first.summary) == ("01D", "c1", "## Docs\n\nFixed the typos.\n- README"))
        #expect(second.sessionId == "01C")
        #expect(!blocks.contains { if case .notice = $0 { true } else { false } })
    }

    @Test func aChildIsDoneOnceATurnCompletedAndFailedWhileItsFailureStands() {
        var script = Script("01C")
        var model = script.model([created(task: "Write tests", parent: "01P")])
        #expect(model.progress == .idle)
        model.apply(SessionUpdate(events: [
            script.event(.sessionStatusChanged(status: .waitingForCapacity, retryAt: nil)),
        ], streaming: []))
        #expect(model.progress == .waiting)
        #expect(model.progress.label == "Waiting for a free slot")
        model.apply(SessionUpdate(events: [
            script.event(.sessionStatusChanged(status: .running, retryAt: nil)), script.event(.turnStarted(turnId: "c1")),
            script.event(.turnCompleted(turnId: "c1")), script.event(.sessionStatusChanged(status: .idle, retryAt: nil)),
        ], streaming: []))
        #expect(model.progress == .done)
        #expect(model.turnEnds["c1"] == .completed)
        model.apply(SessionUpdate(events: [
            script.event(.turnStarted(turnId: "c2")),
            script.event(.turnFailed(turnId: "c2", error: TurnError(class: .fatal, message: "boom"))),
            script.event(.sessionStatusChanged(status: .idle, retryAt: nil)),
        ], streaming: []))
        #expect(model.progress == .failed)
        #expect(model.turnEnds["c2"] == .failed)
    }

    @Test func theSpawnSummaryCountsStatesMostUrgentFirst() {
        #expect(ChildProgress.summary([.done, .running, .needsYou, .running, .failed])
            == "1 needs you · 2 running · 1 failed · 1 done")
        #expect(ChildProgress.summary([.waiting]) == "1 waiting")
        #expect(ChildProgress.summary([]) == "")
    }

    @Test func runTimeCountsTurnsAndTheRunningOne() {
        var script = Script("01C")
        // Events are a second apart: the first turn runs 1s, the second started at 6s.
        let model = script.model([
            created(task: "Write tests", parent: "01P"), .turnStarted(turnId: "c1"), .turnCompleted(turnId: "c1"),
            .sessionStatusChanged(status: .idle, retryAt: nil), .sessionStatusChanged(status: .running, retryAt: nil),
            .turnStarted(turnId: "c2"),
        ])
        let started = Date(timeIntervalSince1970: 1_767_225_600 + 6)
        #expect(model.runTime(at: started.addingTimeInterval(10)) == 11)
    }

    @Test func homeListsActiveChildrenUnderTheirParentAndLeavesIdleOnesOut() {
        var parent = Script("01A")
        var running = Script("01B")
        var done = Script("01C")
        var solo = Script("01D")
        let sessions = [
            parent.key: parent.model([created(task: "Lead")]),
            running.key: running.model([created(task: "Build", parent: "01A"), .sessionStatusChanged(status: .running, retryAt: nil)]),
            done.key: done.model([created(task: "Docs", parent: "01A")]),
            solo.key: solo.model([created(task: "Solo")]),
        ]
        let lists = Lists(
            machines: [machine("host-a", name: "a", sessions: ["01A", "01B", "01C", "01D"])], sessions: sessions)
        // The idle parent leads its running child; the idle child shows only in its project.
        #expect(lists.home.map(\.title) == ["Solo", "Lead", "Build"])
        #expect(lists.home.map(\.depth) == [0, 0, 1])
        #expect(lists.home[1].children == 2)
    }
}
