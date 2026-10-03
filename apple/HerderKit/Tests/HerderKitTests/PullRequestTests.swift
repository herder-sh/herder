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

struct FollowUpTests {
    @Test func aSearchMatchesNameBranchWorktreeAndPullRequests() {
        let pr = PullRequest(number: 103, url: "", title: "Session view", headBranch: nil, state: .open, ci: .none,
                             review: .none, mergeable: .unknown)
        let session = SessionSummary(key: SessionKey(hostId: "h", sessionId: "s"), title: "p7-3-follow-up",
                                     project: "herder", branch: "herder/abc", worktree: "/wt/herder-abc",
                                     machine: "trash-can-01", state: .idle, activity: "", age: "", prs: [pr])
        #expect(session.matches(""))
        #expect(session.matches("FOLLOW"))
        #expect(session.matches("herder/abc"))
        #expect(session.matches("/wt/"))
        #expect(session.matches("#103"))
        #expect(session.matches("session view"))
        #expect(!session.matches("nothing like it"))
    }

    @Test func connectionHealthCountsReconnectsAndDowntime() {
        let start = Date(timeIntervalSince1970: 0)
        let log = [
            ConnectionChange(at: start, state: .connecting),
            ConnectionChange(at: start + 1, state: .connected),
            ConnectionChange(at: start + 61, state: .disconnected(error: "gone")),
            ConnectionChange(at: start + 91, state: .connected),
        ]
        let health = ConnectionHealth(log: log, now: start + 151)
        #expect(health.reconnects == 1)
        #expect(health.down == 30)
        #expect(health.currentUp == 60)
    }

    @Test func theTimelineAndStatsFollowTheEvents() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            .itemAdded(item: Item(id: "c", turnId: "t1", body: .toolCall(name: "Bash", input: "{}"))),
            .approvalRequested(approvalId: "a", turnId: "t1", toolCallId: "c", summary: "Bash: ls", routedTo: .user, reason: nil),
            .approvalResolved(approvalId: "a", decision: .allow, answeredBy: .user),
            .turnCompleted(turnId: "t1"),
        ])
        #expect(model.timeline.map(\.text).last == "Turn completed")
        #expect(model.stats.turns == 1)
        #expect(model.stats.completed == 1)
        #expect(model.stats.tools == ["Bash": 1])
        #expect((model.stats.approvals, model.stats.allowed) == (1, 1))
        #expect(model.stats.busy == 4)
    }

    @Test func newSessionsStartOnClaudeWhenTheMachineHasIt() async throws {
        await MainActor.run {
            guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else { return }
            var host = machine("h", name: "h", sessions: [])
            host.accounts = [
                Account(accountId: "gpt", provider: "codex", label: "gpt", usage: []),
                Account(accountId: "main", provider: "claude", label: "main", usage: []),
            ]
            fleet.setMachinesForTesting([host])
            #expect(fleet.defaultProvider(on: "h", projectId: nil) == "claude")
        }
    }
}

struct ImageAttachmentTests {
    @Test func aSmallPNGGoesAsItIsAndOtherImagesBecomeJPEG() throws {
        // A 1×1 PNG.
        let png = Data(base64Encoded: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==")!
        let image = try ImageAttachment.make(png, type: .png)
        #expect(image.mediaType == "image/png")
        #expect(image.data == png)
        let converted = try ImageAttachment.make(png, type: .tiff)
        #expect(converted.mediaType == "image/jpeg")
        #expect(throws: (any Error).self) { try ImageAttachment.make(Data("not a picture".utf8), type: nil) }
    }
}

struct RoundTwoTests {
    @Test func aProjectWithoutSessionsStillLists() {
        let project = Project(projectId: "github.com/acme/new", name: "new", paths: ["/src/new"], defaultPermissionMode: nil,
                              defaultAccount: nil, setupCommand: nil)
        let lists = Lists(machines: [machine("h", name: "alpha", sessions: [], projects: [project])], sessions: [:])
        #expect(lists.projects.map(\.name) == ["new"])
        #expect(lists.projects[0].machines == ["alpha"])
    }

    @Test func retriesThatFailAlikeFoldIntoOneRun() {
        let start = Date(timeIntervalSince1970: 0)
        let refused = ConnectionState.disconnected(error: "refused")
        let log = [
            ConnectionChange(at: start, state: .connecting),
            ConnectionChange(at: start + 10, state: refused),
            ConnectionChange(at: start + 25, state: .connecting),
            ConnectionChange(at: start + 35, state: refused),
            ConnectionChange(at: start + 50, state: .connecting),
            ConnectionChange(at: start + 60, state: .connected),
        ]
        let runs = ConnectionHealth.runs(log, now: start + 100)
        #expect(runs.map(\.state) == [.connecting, refused, .connecting, .connected])
        #expect(runs[1].times == 2)
    }

    @Test func aProtocolMismatchSaysWhatToDo() {
        let raw = "10.0.0.1:7447: no answer in 10s; 100.1.2.3:7447: protocol version 4 is not supported; this daemon speaks 3"
        #expect(ConnectionState.explain(raw) == "Runs a different herder (protocol 3; this app speaks 4). Update one of them to connect.")
        #expect(ConnectionState.explain("refused") == "refused")
    }
}

struct TypingTests {
    @Test func shiftEnterContinuesListsAndEndsThemOnAnEmptyItem() {
        #expect(ListContinuation.newline(after: "plain") == "plain\n")
        #expect(ListContinuation.newline(after: "1. fix the build") == "1. fix the build\n2. ")
        #expect(ListContinuation.newline(after: "intro\n9. ninth") == "intro\n9. ninth\n10. ")
        #expect(ListContinuation.newline(after: "  - nested") == "  - nested\n  - ")
        #expect(ListContinuation.newline(after: "1. one\n2. ") == "1. one\n")
        #expect(ListContinuation.newline(after: "- ") == "")
    }
}
