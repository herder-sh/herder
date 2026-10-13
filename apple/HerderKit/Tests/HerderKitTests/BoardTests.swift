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
    @Test func theBoardListsLiveTreesInOneListNewestFirst() {
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
        #expect(board.map(\.lead.title) == ["Running", "Failing", "Ready"])
        #expect(board[2].children.map(\.title) == ["Ready too"])
    }

    /// The Board of `sessions`, all in one project on one machine.
    private func board(_ sessions: [SessionModel]) -> [BoardTree] {
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
        #expect(columns.map(\.lead.title) == ["Lead"])
        #expect(columns[0].children.map(\.title) == ["Child"])
    }

    @Test func aTreesAgentsRollUpToTheirMostPressingState() {
        var parent = Script("01A")
        var asking = Script("01B")
        var running = Script("01C")
        var archived = Script("01D")
        let trees = board([
            parent.model([created(task: "Lead"), .prLinked(pr: pr(1, ci: .failing))]),
            asking.model([
                created(task: "Asking", parent: "01A"),
                .approvalRequested(approvalId: "a", turnId: "t", toolCallId: "i", summary: "s", routedTo: .user, reason: nil),
            ]),
            running.model([created(task: "Running", parent: "01A"), .sessionStatusChanged(status: .running, retryAt: nil)]),
            // An archived child leaves the board.
            archived.model([created(task: "Archived", parent: "01A"), .sessionStatusChanged(status: .archived, retryAt: nil)]),
        ])
        #expect(trees.count == 1)
        #expect(trees[0].agentCount == 2)
        #expect(trees[0].agentsState == .needsYou)

        var lone = Script("01E")
        let alone = board([lone.model([created(task: "Alone")])])
        #expect(alone[0].agentCount == 0)
        #expect(alone[0].agentsState == nil)
    }

    @Test func archivedChildrenLeaveTheBoardAndAnArchivedParentStillLeadsItsLiveChild() {
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
        let trees = columns
        #expect(trees.map(\.lead.title) == ["Archived lead", "Lead"])
        #expect(trees[0].lead.state == .archived)
        #expect(trees[0].listed.map(\.title) == ["Its live child"])
        // Archived children no live session hangs from are in the project's Archived group.
        #expect(trees[1].listed.map(\.title) == ["Live"])
        #expect(trees[1].archived.isEmpty)
    }

    @Test func aSearchFindsATreeByAnyOfItsSessions() {
        var parent = Script("01A")
        var child = Script("01B")
        let tree = board([
            parent.model([created(task: "Lead")]), child.model([created(task: "Unlock deal pages", parent: "01A")]),
        ])[0]
        #expect(tree.matches("deal pages"))
        #expect(tree.matches("lead"))
        #expect(!tree.matches("nothing"))
    }

    @Test func aSearchKeepsTheTreesItFinds() {
        var deal = Script("01A")
        var child = Script("01B")
        var running = Script("01C")
        let sessions = [
            deal.key: deal.model([created(task: "Fix the deal pages"), .prLinked(pr: pr(1))]),
            child.key: child.model([created(task: "Style the checkout", parent: "01A", branch: "herder/checkout")]),
            running.key: running.model([created(task: "Run the tests"), .sessionStatusChanged(status: .running, retryAt: nil)]),
        ]
        let lists = Lists(machines: [machine("host-a", name: "a", sessions: ["01A", "01B", "01C"])], sessions: sessions)
        #expect(lists.board(matching: "").map(\.lead.title) == ["Run the tests", "Fix the deal pages"])
        // A child's branch finds its whole tree.
        #expect(lists.board(matching: "CHECKOUT").map(\.lead.title) == ["Fix the deal pages"])
        #expect(lists.board(matching: "tests").map(\.lead.title) == ["Run the tests"])
        #expect(lists.board(matching: "nothing").isEmpty)
    }
}
