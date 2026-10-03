import Foundation
import Herder
@testable import HerderKit
import Testing

private func pr(_ number: UInt64, _ state: PrState) -> PullRequest {
    PullRequest(number: number, url: "https://github.com/acme/demo/pull/\(number)", title: "PR \(number)",
                headBranch: "b\(number)", state: state, ci: .passing, review: .none, mergeable: .clean)
}

struct PullRequestTests {
    @Test func aPRIsLinkedByNumberOrLinkAsInTheTUI() {
        #expect(prNumber(in: "123") == 123)
        #expect(prNumber(in: " #42 ") == 42)
        #expect(prNumber(in: "https://github.com/herder-sh/herder/pull/96/files") == 96)
        #expect(prNumber(in: "pull request") == nil)
    }

    @Test func pullRequestsGroupByProjectWithOpenOnesFirst() {
        var withPRs = Script("01A")
        var without = Script("01B")
        let sessions = [
            withPRs.key: withPRs.model([created(task: "Ship"), .prLinked(pr: pr(1, .merged)), .prLinked(pr: pr(2, .open))]),
            without.key: without.model([created(task: "Idle")]),
        ]
        let lists = Lists(machines: [machine("host-a", name: "a", sessions: ["01A", "01B"])], sessions: sessions)
        let all = lists.pullRequests(openOnly: false)
        #expect(all.count == 1)
        #expect(all[0].sessions.map(\.session.title) == ["Ship"])
        #expect(all[0].sessions[0].prs.map(\.number) == [2, 1])
        #expect(lists.pullRequests(openOnly: true)[0].sessions[0].prs.map(\.number) == [2])
    }
}
