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
        #expect(board.map { $0.trees.map(\.lead.title) } == [["Failing"], ["Ready"], ["Running"]])
        #expect(board[1].trees[0].children.map(\.title) == ["Ready too"])
    }

    /// The Board of `sessions`, all in one project on one machine.
    private func board(_ sessions: [SessionModel]) -> [BoardColumn] {
        var host = machine("host-a", name: "a", sessions: sessions.map(\.key.sessionId))
        for index in host.sessions.indices { host.sessions[index].projectId = "github.com/acme/app" }
        return Lists(machines: [host], sessions: Dictionary(uniqueKeysWithValues: sessions.map { ($0.key, $0) })).board
    }

    @Test func aChildIsListedUnderItsParentNotOnItsOwn() {
        var parent = Script("01A")
        var child = Script("01B")
        let columns = board([
            parent.model([created(task: "Lead"), .prLinked(pr: pr(1))]),
            child.model([created(task: "Child", parent: "01A"), .prLinked(pr: pr(2))]),
        ])
        #expect(columns.map(\.state) == [.readyToMerge])
        #expect(columns[0].trees.map(\.lead.title) == ["Lead"])
        #expect(columns[0].trees[0].children.map(\.title) == ["Child"])
    }

    @Test func aTreeStandsWhereItsMostUrgentLiveSessionDoes() {
        var parent = Script("01A")
        var asking = Script("01B")
        var failing = Script("01C")
        var archived = Script("01D")
        let columns = board([
            parent.model([created(task: "Lead"), .sessionStatusChanged(status: .running, retryAt: nil)]),
            asking.model([
                created(task: "Asking", parent: "01A"),
                .approvalRequested(approvalId: "a", turnId: "t", toolCallId: "i", summary: "s", routedTo: .user, reason: nil),
            ]),
            failing.model([created(task: "Failing", parent: "01A"), .prLinked(pr: pr(1, ci: .failing))]),
            // An archived child's work no longer counts.
            archived.model([
                created(task: "Archived", parent: "01A"), .prLinked(pr: pr(2, ci: .failing)),
                .sessionStatusChanged(status: .archived, retryAt: nil),
            ]),
        ])
        // A child that needs you does not hide in its parent's Working column.
        #expect(columns.map(\.state) == [.needsYou])
        #expect(columns[0].trees.count == 1)

        var lead = Script("01E")
        var child = Script("01F")
        let failingChild = board([
            lead.model([created(task: "Lead"), .sessionStatusChanged(status: .running, retryAt: nil)]),
            child.model([created(task: "Child", parent: "01E"), .prLinked(pr: pr(3, ci: .failing))]),
        ])
        #expect(failingChild.map(\.state) == [.ciFailed])
    }

    @Test func archivedChildrenAreListedLastAndAnArchivedParentStillLeadsItsLiveChild() {
        let archive = EventBody.sessionStatusChanged(status: .archived, retryAt: nil)
        var scripts = ["01A", "01B", "01C", "01D", "01E", "01F"].map { Script($0) }
        let columns = board([
            scripts[0].model([created(task: "Lead")]),
            scripts[1].model([created(task: "Archived first", parent: "01A"), archive]),
            scripts[2].model([created(task: "Live", parent: "01A")]),
            scripts[3].model([created(task: "Archived second", parent: "01A"), archive]),
            scripts[4].model([created(task: "Archived lead"), archive]),
            scripts[5].model([created(task: "Its live child", parent: "01E")]),
        ])
        #expect(columns.map(\.state) == [.idle])
        let trees = columns[0].trees
        #expect(trees.map(\.lead.title) == ["Archived lead", "Lead"])
        #expect(trees[0].lead.state == .archived)
        #expect(trees[0].listed.map(\.title) == ["Its live child"])
        #expect(trees[1].listed.map(\.title) == ["Live", "Archived first", "Archived second"])
        #expect(trees[1].archived.map(\.title) == ["Archived first", "Archived second"])
    }

    @Test func aSearchFindsATreeByAnyOfItsSessions() {
        var parent = Script("01A")
        var child = Script("01B")
        let tree = board([
            parent.model([created(task: "Lead")]), child.model([created(task: "Unlock deal pages", parent: "01A")]),
        ])[0].trees[0]
        #expect(tree.matches("deal pages"))
        #expect(tree.matches("lead"))
        #expect(!tree.matches("nothing"))
    }
}
