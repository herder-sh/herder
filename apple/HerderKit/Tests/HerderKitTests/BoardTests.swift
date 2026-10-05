import Foundation
import Herder
@testable import HerderKit
import Testing

private func pr(
    _ number: UInt64, _ state: PrState = .open, ci: CiStatus = .passing, review: ReviewStatus = .none,
    mergeable: Mergeable = .clean
) -> PullRequest {
    PullRequest(number: number, url: "https://github.com/acme/demo/pull/\(number)", title: "PR \(number)",
                headBranch: "b\(number)", state: state, ci: ci, review: review, mergeable: mergeable)
}

struct BoardTests {
    @Test func whatTheSessionDoesComesBeforeItsPRs() {
        let failing = [pr(1, ci: .failing)]
        #expect(WorkState(state: .needsYou, prs: failing) == .needsYou)
        #expect(WorkState(state: .error, prs: []) == .needsYou)
        #expect(WorkState(state: .running, prs: failing) == .working)
        #expect(WorkState(state: .waiting, prs: []) == .working)
    }

    @Test func aStoppedSessionStandsWhereItsOpenPRDoes() {
        #expect(WorkState(state: .idle, prs: [pr(1, ci: .failing)]) == .ciFailed)
        #expect(WorkState(state: .idle, prs: [pr(1, review: .changesRequested)]) == .changesRequested)
        #expect(WorkState(state: .idle, prs: [pr(1, mergeable: .conflicting)]) == .conflicting)
        #expect(WorkState(state: .idle, prs: [pr(1, ci: .pending)]) == .waitingOnCI)
        // GitHub has not worked out whether it merges yet.
        #expect(WorkState(state: .idle, prs: [pr(1, mergeable: .unknown)]) == .waitingOnCI)
        #expect(WorkState(state: .idle, prs: [pr(1)]) == .readyToMerge)
        // A repository without checks merges on review and mergeability alone.
        #expect(WorkState(state: .idle, prs: [pr(1, ci: .none)]) == .readyToMerge)
        // Done is idle, unseen.
        #expect(WorkState(state: .done, prs: [pr(1)]) == .readyToMerge)
    }

    @Test func aDraftIsNotReadyToMerge() {
        #expect(WorkState(state: .idle, prs: [pr(1, .draft)]) == .idle)
        #expect(WorkState(state: .idle, prs: [pr(1, .draft, ci: .failing)]) == .ciFailed)
    }

    @Test func aFailingCheckOutranksTheRestOfAPR() {
        #expect(WorkState(pr: pr(1, ci: .failing, review: .changesRequested, mergeable: .conflicting)) == .ciFailed)
        #expect(WorkState(pr: pr(2, review: .changesRequested, mergeable: .conflicting)) == .changesRequested)
    }

    @Test func theMostUrgentOpenPRWinsAndFinishedOnesOnlyCountWithoutOne() {
        #expect(WorkState(state: .idle, prs: [pr(1), pr(2, mergeable: .conflicting), pr(3, .merged)]) == .conflicting)
        #expect(WorkState(state: .idle, prs: [pr(1, .merged), pr(2, .closed)]) == .merged)
        #expect(WorkState(state: .idle, prs: [pr(1, .closed)]) == .idle)
        #expect(WorkState(state: .idle, prs: []) == .idle)
    }

    @Test func attentionComesFirstAndMergedLast() {
        #expect(WorkState.allCases == [
            .needsYou, .ciFailed, .changesRequested, .conflicting, .readyToMerge, .idle, .working, .waitingOnCI, .merged,
        ])
        #expect(WorkState.allCases.sorted() == WorkState.allCases)
    }

    @Test func theBoardGroupsLiveSessionsByWorkStateNewestFirst() {
        var ready = Script("01A")
        var failing = Script("01B")
        var running = Script("01C")
        var archived = Script("01D")
        var readyToo = Script("01E")
        let sessions = [
            ready.key: ready.model([created(task: "Ready"), .prLinked(pr: pr(1))]),
            failing.key: failing.model([created(task: "Failing"), .prLinked(pr: pr(2, ci: .failing))]),
            running.key: running.model([created(task: "Running"), .sessionStatusChanged(status: .running, retryAt: nil)]),
            archived.key: archived.model([
                created(task: "Archived"), .prLinked(pr: pr(3)), .sessionStatusChanged(status: .archived, retryAt: nil),
            ]),
            readyToo.key: readyToo.model([created(task: "Ready too", parent: "01A"), .prLinked(pr: pr(4))]),
        ]
        let lists = Lists(machines: [machine("host-a", name: "a", sessions: ["01A", "01B", "01C", "01D", "01E"])],
                          sessions: sessions)
        let board = lists.board
        #expect(board.map(\.state) == [.ciFailed, .readyToMerge, .working])
        #expect(board.map { $0.sessions.map(\.title) } == [["Failing"], ["Ready too", "Ready"], ["Running"]])
        // A child stands on its own on the Board, not under its parent.
        #expect(board.flatMap(\.sessions).allSatisfy { $0.depth == 0 })
    }
}
