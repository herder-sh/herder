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

    mutating func event(_ body: EventBody, by: UserId? = nil) -> Event {
        seq += 1
        let at = Date(timeIntervalSince1970: 1_767_225_600 + Double(seq))
        return Event(sessionId: key.sessionId, seq: seq, at: at.ISO8601Format(), by: by, body: body)
    }

    mutating func model(_ bodies: [EventBody]) -> SessionModel {
        var model = SessionModel(key: key)
        model.apply(SessionUpdate(events: bodies.map { event($0) }, streaming: []))
        return model
    }
}

func created(task: String? = nil, parent: SessionId? = nil, branch: String = "herder/abc", model: String = "opus") -> EventBody {
    .sessionCreated(
        repo: "/src/demo", worktree: "/wt/demo", branch: branch, provider: "claude", accountId: "main",
        model: model, permissionMode: .ask, parent: parent, parentHost: nil, task: task, failoverPin: nil, chat: false)
}

struct SessionModelTests {
    @Test func aGeneratedTitleNamesTheSessionOverItsBranch() {
        var script = Script()
        var model = script.model([created()])
        #expect(model.title == "herder/abc")
        model.apply(script.event(.titleChanged(title: "Start iOS App Development", source: .auto)))
        #expect(model.title == "Start iOS App Development")
    }

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
            .questionAsked(questionId: "q1", turnId: "t1", text: "Which?", choices: ["A", "B"], multiSelect: false, routedTo: .primary, reason: nil),
        ])
        #expect(!model.needsUser)
        model.apply(script.event(.questionEscalated(questionId: "q1", reason: .timeout, note: "no idea")))
        #expect(model.needsUser)
        #expect(model.forUser.first?.note == "no idea")
        #expect(model.title == "Write tests")
    }

    @Test func aSessionWhoseAccountNeedsLoginNeedsTheUser() {
        var script = Script()
        let model = script.model([
            created(), .sessionStatusChanged(status: .running, retryAt: nil), .turnStarted(turnId: "t1"),
            .turnFailed(turnId: "t1", error: TurnError(class: .auth, message: "Not logged in")),
            .sessionStatusChanged(status: .needsLogin, retryAt: nil),
        ])
        #expect(model.needsUser)
        #expect(model.state == .needsYou)
    }

    @Test func aTurnsQuestionsGoWhenItEnds() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            .questionAsked(questionId: "q1", turnId: "t1", text: "Which?", choices: [], multiSelect: false, routedTo: .user, reason: nil),
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

    @Test func questionsAskedTogetherCountTheAnsweredOnes() {
        var script = Script()
        func ask(_ id: String) -> EventBody {
            .questionAsked(questionId: id, turnId: "t1", text: "Which?", choices: ["Red", "Blue"], multiSelect: false, routedTo: .user, reason: nil)
        }
        func answer(_ id: String) -> EventBody {
            .questionAnswered(questionId: id, answer: .choice(index: 0), answeredBy: .user)
        }
        var model = script.model([created(), .turnStarted(turnId: "t1"), ask("q1"), ask("q2"), ask("q3"), answer("q1")])
        #expect(model.questionRun == 3 && model.questions.count == 2)
        model.apply(script.event(answer("q2")))
        model.apply(script.event(answer("q3")))
        model.apply(script.event(ask("q4")))
        #expect(model.questionRun == 1)
    }

    @Test func aMultiSelectQuestionIsPinnedAsOne() throws {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            .questionAsked(questionId: "q1", turnId: "t1", text: "Which checks?", choices: ["Lint", "Tests"],
                           multiSelect: true, routedTo: .user, reason: nil),
        ])
        let summary = SessionSummary(
            key: SessionKey(hostId: "h1", sessionId: "s1"), title: "CI", project: "", branch: "", machine: "box",
            state: model.state, activity: model.activity, age: "now")
        let pinned = try #require(model.pinned(summary, userOnly: true))
        #expect(pinned.kind == .question(text: "Which checks?", choices: ["Lint", "Tests"], multiSelect: true))
    }

    /// The card's ticks: each tap ticks or unticks a choice, and Submit sends them in order.
    @Test func ticksOnAMultiSelectCardSubmitTogether() {
        var picks = Picks()
        #expect(picks.answer == nil)
        for index in [2, 0, 1, 1] { picks.toggle(index) }
        #expect(picks.answer == .choices(indices: [0, 2]))
        picks.toggle(0)
        picks.toggle(2)
        #expect(picks.answer == nil)
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
            .turnCompleted(turnId: "t1", usage: nil), .sessionStatusChanged(status: .idle, retryAt: nil),
        ])
        #expect(model.lastTool == "Running ls")
        #expect(model.activity == "Done.")
    }

    @Test func aToolCallsActivityIsWordsNotItsIdentifier() {
        #expect(toolActivity(name: "Bash", input: #"{"command":"cargo test --workspace"}"#) == "Running cargo")
        #expect(toolActivity(name: "Bash", input: "{}") == "Running a command")
        #expect(toolActivity(name: "Read", input: #"{"file_path":"/repo/apple/Board.swift"}"#) == "Reading Board.swift")
        #expect(toolActivity(name: "Edit", input: #"{"file_path":"src/lib.rs"}"#) == "Editing lib.rs")
        #expect(toolActivity(name: "Grep", input: #"{"pattern":"TaskListRow"}"#) == "Searching for TaskListRow")
        #expect(toolActivity(name: "WebFetch", input: #"{"url":"https://docs.rs/serde"}"#) == "Fetching docs.rs")
        #expect(toolActivity(name: "Task", input: #"{"description":"Review the diff"}"#) == "Starting an agent: Review the diff")
        #expect(toolActivity(name: "mcp__herder__spawn", input: #"{"task":"Helm"}"#) == "Starting an agent")
        #expect(toolActivity(name: "mcp__herder__spawn", input: "{}", spawned: 3) == "Starting 3 agents")
        #expect(toolActivity(name: "mcp__herder__wait_for", input: "{}") == "Waiting for agents")
        #expect(toolActivity(name: "mcp__linear__create_issue", input: "{}") == "Using create issue on linear")
        #expect(toolActivity(name: "TodoWrite", input: "{}") == "Updating its plan")
        #expect(toolActivity(name: "exit_plan_mode", input: "{}") == "Using exit plan mode")
    }

    @Test func aRunOfSpawnsSaysHowManyAgentsItStarted() {
        var script = Script()
        func spawn(_ id: String) -> EventBody {
            .itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: id, turnId: "t1",
                                  body: .toolCall(name: "mcp__herder__spawn", input: #"{"task":"x"}"#)))
        }
        var model = script.model([created(), .turnStarted(turnId: "t1"), spawn("i1"), spawn("i2"), spawn("i3")])
        #expect(model.lastTool == "Starting 3 agents")
        model.apply(script.event(.itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: "i4", turnId: "t1",
                                                       body: .toolCall(name: "mcp__herder__wait_for", input: "{}")))))
        #expect(model.lastTool == "Waiting for agents")
    }
}
