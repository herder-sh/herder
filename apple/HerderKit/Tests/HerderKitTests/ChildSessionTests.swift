import Foundation
import Herder
@testable import HerderKit
import Testing

struct ChildSessionTests {
    @Test func aReportIsABlockTiedToItsChildAfterTheSpawn() {
        var script = Script("01P")
        let model = script.model([
            created(task: "Lead"), .turnStarted(turnId: "t1"),
            .childSpawned(childSessionId: "01C", hostId: nil, task: "Write tests"),
            .childSpawned(childSessionId: "01D", hostId: nil, task: "Fix docs"),
            .itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: "a", turnId: "t1", body: .assistantMessage(text: "Waiting."))),
            .childReported(childSessionId: "01D", turnId: "c1", summary: "## Docs\n\nFixed the typos.\n- README"),
            .childReported(childSessionId: "01C", turnId: "c1", summary: "Tests pass."),
        ])
        let blocks = Transcript.blocks(model).filter { if case .working = $0 { false } else { true } }
        guard blocks.count == 4, case .children(_, let children) = blocks[0],
              case .report(let first, _) = blocks[2], case .report(let second, _) = blocks[3] else {
            Issue.record("unexpected blocks: \(blocks)")
            return
        }
        #expect(children.map(\.sessionId) == ["01C", "01D"])
        #expect((first.sessionId, first.turnId, first.summary) == ("01D", "c1", "## Docs\n\nFixed the typos.\n- README"))
        #expect(second.sessionId == "01C")
        #expect(!blocks.contains { if case .notice = $0 { true } else { false } })
    }

    @Test func aReportIsSupersededOnceTheSameChildReportsAgain() {
        var script = Script("01P")
        let model = script.model([
            created(task: "Lead"),
            .childReported(childSessionId: "01C", turnId: "c1", summary: "First pass."),
            .childReported(childSessionId: "01D", turnId: "d1", summary: "Docs fixed."),
            .childReported(childSessionId: "01C", turnId: "c2", summary: "Second pass."),
        ])
        let reports = Transcript.blocks(model).compactMap { block -> (String, Bool)? in
            guard case .report(let report, let superseded) = block else { return nil }
            return (report.summary, superseded)
        }
        #expect(reports.map(\.0) == ["First pass.", "Docs fixed.", "Second pass."])
        #expect(reports.map(\.1) == [true, false, false])
    }

    @Test func anEarlierReportPreviewsItsFirstLinePastTheHeadings() {
        func preview(_ summary: String) -> String {
            ChildReport(id: 1, sessionId: "01C", turnId: "c1", summary: summary).preview
        }
        #expect(preview("## Docs\n\nFixed the typos.") == "Fixed the typos.")
        #expect(preview("# Done") == "Done")
        #expect(preview("\n#1922 is still a draft.\nMore.") == "#1922 is still a draft.")
        #expect(preview("Tests pass.") == "Tests pass.")
    }

    @Test func aReportShowsWhereTheChildStandsNotThatItsTurnEnded() {
        func pr(_ number: UInt64, _ state: PrState, ci: CiStatus = .none) -> PullRequest {
            PullRequest(number: number, url: "https://github.com/acme/demo/pull/\(number)", title: "PR", headBranch: nil,
                        state: state, ci: ci, review: .none, mergeable: .clean)
        }
        // A completed turn says nothing about the task: the child's state and PR do.
        #expect(ReportStatus(progress: .done, prs: [pr(1922, .draft)], end: .completed).text == "Idle · #1922 draft")
        #expect(ReportStatus(progress: .running, prs: [], end: .completed).text == "Working")
        #expect(ReportStatus(progress: .done, prs: [pr(1921, .open, ci: .pending)], end: .completed).text
            == "Idle · #1921 CI running")
        #expect(ReportStatus(progress: .done, prs: [pr(1921, .merged)], end: .completed).text == "Idle · #1921 merged")
        // The open PR leads, newest first, and the rest count.
        #expect(ReportStatus(progress: .done, prs: [pr(10, .merged), pr(11, .open), pr(12, .open, ci: .failing)],
                             end: .completed).text == "Idle · #12 CI failing +2")
        // A failed or interrupted turn shows how it ended; a failed child does not repeat it.
        #expect(ReportStatus(progress: .failed, prs: [], end: .failed).text == "Failed")
        #expect(ReportStatus(progress: .done, prs: [], end: .interrupted).text == "Interrupted · Idle")
        #expect(ReportStatus(progress: nil, prs: [], end: .completed).text == "")
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
            script.event(.turnCompleted(turnId: "c1", usage: nil)), script.event(.sessionStatusChanged(status: .idle, retryAt: nil)),
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
            created(task: "Write tests", parent: "01P"), .turnStarted(turnId: "c1"), .turnCompleted(turnId: "c1", usage: nil),
            .sessionStatusChanged(status: .idle, retryAt: nil), .sessionStatusChanged(status: .running, retryAt: nil),
            .turnStarted(turnId: "c2"),
        ])
        let started = Date(timeIntervalSince1970: 1_767_225_600 + 6)
        #expect(model.runTime(at: started.addingTimeInterval(10)) == 11)
    }

    private func report(_ id: UInt64, _ child: SessionId) -> ChildReport {
        ChildReport(id: id, sessionId: child, turnId: "t\(id)", summary: "Report \(id).")
    }

    @Test func aRunOfEarlierReportsGroupsIntoOneBlockWhereItWas() {
        let reply = TranscriptBlock.assistant(id: "a", text: "Waiting.", streaming: false)
        let earlier = [report(1, "01C"), report(2, "01D"), report(3, "01C")]
        let latest = [report(4, "01C"), report(5, "01D")]
        let blocks = Transcript.groupingEarlierReports(
            [reply] + earlier.map { .report($0, superseded: true) } + latest.map { .report($0, superseded: false) })
        #expect(blocks == [reply, .earlierReports(id: "earlier-reports-1", earlier),
                           .report(latest[0], superseded: false), .report(latest[1], superseded: false)])
    }

    @Test func aLoneEarlierReportAndBlocksBetweenReportsStayAsTheyAre() {
        let reply = TranscriptBlock.assistant(id: "a", text: "Waiting.", streaming: false)
        let blocks: [TranscriptBlock] = [
            .report(report(1, "01C"), superseded: true), reply,
            .report(report(2, "01C"), superseded: true), .report(report(3, "01D"), superseded: true),
            .report(report(4, "01C"), superseded: false), .report(report(5, "01D"), superseded: true),
        ]
        #expect(Transcript.groupingEarlierReports(blocks) == [
            blocks[0], reply, .earlierReports(id: "earlier-reports-2", [report(2, "01C"), report(3, "01D")]),
            blocks[4], blocks[5],
        ])
    }

    @Test func aParentsTranscriptGroupsTheReportsItsFoldedWorkLeavesSideBySide() {
        var script = Script("01P")
        func item(_ id: String, _ body: ItemBody) -> EventBody {
            .itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: id, turnId: "t1", body: body))
        }
        let model = script.model([
            created(task: "Lead"), .turnStarted(turnId: "t1"),
            item("w1", .toolCall(name: "mcp__herder__wait_for", input: "{}")),
            .childReported(childSessionId: "01C", turnId: "c1", summary: "First pass."),
            item("r1", .toolResult(callId: "w1", output: "{}", isError: false)),
            item("w2", .toolCall(name: "mcp__herder__wait_for", input: "{}")),
            .childReported(childSessionId: "01C", turnId: "c2", summary: "Second pass."),
            item("r2", .toolResult(callId: "w2", output: "{}", isError: false)),
            item("a", .assistantMessage(text: "Both are on it.")),
            .turnCompleted(turnId: "t1", usage: nil),
            .childReported(childSessionId: "01C", turnId: "c3", summary: "Third pass."),
        ])
        let blocks = Transcript.collapsed(model)
        guard blocks.count == 4, case .earlierReports(_, let earlier) = blocks[0],
              case .work = blocks[1], case .assistant = blocks[2], case .report(let latest, superseded: false) = blocks[3] else {
            Issue.record("unexpected blocks: \(blocks)")
            return
        }
        #expect(earlier.map(\.summary) == ["First pass.", "Second pass."])
        #expect(latest.summary == "Third pass.")
    }
}
