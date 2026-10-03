import Foundation
import Herder
@testable import HerderKit
import Testing

/// Events for one session, a second apart from 2026-01-01T00:00:00Z.
struct Script {
    var key = SessionKey(hostId: "host-a", sessionId: "01A")
    private var seq: UInt64 = 0

    init(_ sessionId: String = "01A", host: String = "host-a") {
        key = SessionKey(hostId: host, sessionId: sessionId)
    }

    mutating func event(_ body: EventBody) -> Event {
        seq += 1
        let at = Date(timeIntervalSince1970: 1_767_225_600 + Double(seq))
        return Event(sessionId: key.sessionId, seq: seq, at: at.ISO8601Format(), by: nil, body: body)
    }

    mutating func model(_ bodies: [EventBody]) -> SessionModel {
        var model = SessionModel(key: key)
        model.apply(SessionUpdate(events: bodies.map { event($0) }, streaming: []))
        return model
    }
}

func created(task: String? = nil, parent: SessionId? = nil, branch: String = "herder/abc") -> EventBody {
    .sessionCreated(
        repo: "/src/demo", worktree: "/wt/demo", branch: branch, provider: "claude", accountId: "main",
        model: "opus", permissionMode: .ask, parent: parent, task: task, maxChildren: nil, failoverPin: nil)
}

struct SessionModelTests {
    @Test func statusComesFromStatusEvents() {
        var script = Script()
        let model = script.model([created(), .sessionStatusChanged(status: .running, retryAt: nil), .turnStarted(turnId: "t1")])
        #expect(model.state == .running)
        #expect(model.title == "herder/abc")
        #expect(model.turn == "t1")
    }

    @Test func anApprovalForTheUserNeedsThemUntilResolved() {
        var script = Script()
        var model = script.model([
            created(), .sessionStatusChanged(status: .running, retryAt: nil), .turnStarted(turnId: "t1"),
            .approvalRequested(approvalId: "a1", turnId: "t1", toolCallId: "i1", summary: "Bash: ls", routedTo: .user, reason: nil),
        ])
        #expect(model.state == .needsYou)
        #expect(model.activity == "Bash: ls")
        model.apply(script.event(.approvalResolved(approvalId: "a1", decision: .allow, answeredBy: .user)))
        #expect(model.state == .running)
    }

    @Test func aRequestForThePrimaryNeedsTheUserOnlyOnceEscalated() {
        var script = Script()
        var model = script.model([
            created(task: "Write tests", parent: "01P"), .sessionStatusChanged(status: .running, retryAt: nil),
            .turnStarted(turnId: "t1"),
            .questionAsked(questionId: "q1", turnId: "t1", text: "Which?", choices: ["A", "B"], routedTo: .primary, reason: nil),
        ])
        #expect(!model.needsUser)
        model.apply(script.event(.questionEscalated(questionId: "q1", reason: .timeout, note: "no idea")))
        #expect(model.needsUser)
        #expect(model.forUser.first?.note == "no idea")
        #expect(model.title == "Write tests")
    }

    @Test func aTurnsQuestionsGoWhenItEnds() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            .questionAsked(questionId: "q1", turnId: "t1", text: "Which?", choices: [], routedTo: .user, reason: nil),
            .turnInterrupted(turnId: "t1"), .sessionStatusChanged(status: .idle, retryAt: nil),
        ])
        #expect(model.questions.isEmpty)
        #expect(model.turn == nil)
        #expect(model.state == .idle)
    }

    @Test func aFailedTurnSaysWhy() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            .turnFailed(turnId: "t1", error: TurnError(class: .limitReached, message: "usage limit reached\nretry later")),
            .sessionStatusChanged(status: .error, retryAt: nil),
        ])
        #expect(model.activity == "Turn failed: usage limit reached")
    }

    @Test func pullRequestsAreUpsertedByNumber() {
        var script = Script()
        func pr(_ state: PrState) -> PullRequest {
            PullRequest(number: 7, url: "https://example.com/7", title: "Fix", headBranch: "fix", state: state,
                        ci: .pending, review: .none, mergeable: .unknown)
        }
        var model = script.model([created(), .prLinked(pr: pr(.open)), .prUpdated(pr: pr(.merged))])
        #expect(model.prs.map(\.state) == [.merged])
        model.apply(script.event(.prUnlinked(number: 7)))
        #expect(model.prs.isEmpty)
    }

    @Test func theLastReplyIsWhatAnIdleSessionShows() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            .itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: "i1", turnId: "t1", body: .toolCall(name: "Bash", input: #"{"command":"ls -la"}"#))),
            .itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: "i2", turnId: "t1", body: .assistantMessage(text: "Done.\nMore detail."))),
            .turnCompleted(turnId: "t1"), .sessionStatusChanged(status: .idle, retryAt: nil),
        ])
        #expect(model.lastTool == "Bash ls -la")
        #expect(model.activity == "Done.")
    }
}
