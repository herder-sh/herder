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
    @Test func archivingATaskOpensTheOneBelowElseTheOneAbove() {
        let (a, b, c) = (SessionKey(hostId: "h", sessionId: "01A"), SessionKey(hostId: "h", sessionId: "01B"),
                         SessionKey(hostId: "h", sessionId: "01C"))
        // In the middle and first: the one below.
        #expect(TaskTree.next(after: b, in: [a, b, c]) == c)
        #expect(TaskTree.next(after: a, in: [a, b, c]) == b)
        // Last: the one above.
        #expect(TaskTree.next(after: c, in: [a, b, c]) == b)
        // The only one, or one the list does not show: nothing.
        #expect(TaskTree.next(after: a, in: [a]) == nil)
        #expect(TaskTree.next(after: a, in: [b, c]) == nil)
        // A search hiding B: the next the list shows, not the next of all.
        #expect(TaskTree.next(after: a, in: [a, c]) == c)
    }

    @Test func theNextTaskFollowsTheBoardsSearchAndSort() {
        var alpha = Script("01A")
        var beta = Script("01B")
        var alphaToo = Script("01C")
        let lists = Lists(machines: [machine("host-a", name: "a", sessions: ["01A", "01B", "01C"])], sessions: [
            alpha.key: alpha.model([created(task: "Alpha fix")]),
            beta.key: beta.model([created(task: "Beta")]),
            alphaToo.key: alphaToo.model([created(task: "Alpha docs")]),
        ])
        let shown = BoardSection.sections(lists.board(matching: "alpha"), by: .recent, now: .now).flatMap(\.trees)
        #expect(shown.map(\.lead.title) == ["Alpha docs", "Alpha fix"])
        #expect(TaskTree.next(after: alphaToo.key, in: shown.map(\.id)) == alpha.key)
    }

    @Test func theBoardListsOpenTreesNewestFirst() {
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
        let board = BoardSection.sections(lists.board(), by: .recent, now: .now).flatMap(\.trees)
        #expect(board.map(\.lead.title) == ["Running", "Failing", "Ready"])
        #expect(board[2].children.map(\.title) == ["Ready too"])
    }

    /// The Board of `sessions`, all in one project on one machine.
    private func board(_ sessions: [SessionModel]) -> [TaskTree] {
        projectLists(sessions).board()
    }

    /// The lists of `sessions`, all in one project on one machine.
    private func projectLists(_ sessions: [SessionModel]) -> Lists {
        var host = machine("host-a", name: "a", sessions: sessions.map(\.key.sessionId))
        for index in host.sessions.indices { host.sessions[index].projectId = "github.com/acme/app" }
        return Lists(machines: [host], sessions: Dictionary(uniqueKeysWithValues: sessions.map { ($0.key, $0) }))
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

    @Test func anArchivedTopTakesItsTaskOffTheBoardAndArchivedChildrenGetNoRow() {
        let archive = EventBody.sessionStatusChanged(status: .archived, retryAt: nil)
        var scripts = ["01A", "01B", "01C", "01D", "01E", "01F"].map { Script($0) }
        let trees = board([
            scripts[0].model([created(task: "Lead")]),
            scripts[1].model([created(task: "Archived first", parent: "01A"), archive]),
            scripts[2].model([created(task: "Live", parent: "01A")]),
            scripts[3].model([created(task: "Archived second", parent: "01A"), archive]),
            scripts[4].model([created(task: "Archived lead"), archive]),
            scripts[5].model([created(task: "Its live child", parent: "01E")]),
        ])
        #expect(trees.map(\.lead.title) == ["Lead"])
        #expect(trees[0].listed.map(\.title) == ["Live"])
    }

    @Test func showArchivedListsTheTasksArchivedTodayAndNoChild() {
        let archive = EventBody.sessionStatusChanged(status: .archived, retryAt: nil)
        var scripts = ["01A", "01B", "01C", "01D"].map { Script($0) }
        let lists = projectLists([
            scripts[0].model([created(task: "Open")]),
            scripts[1].model([created(task: "Archived child", parent: "01A"), archive]),
            scripts[2].model([created(task: "Done"), archive]),
            scripts[3].model([created(task: "Done child", parent: "01C"), archive]),
        ])
        // The scripts' events are at the start of 2026, UTC.
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = .gmt
        let today = Date(timeIntervalSince1970: 1_767_225_600 + 3600)
        #expect(lists.board().map(\.lead.title) == ["Open"])
        let archived = lists.archivedToday(now: today, calendar: calendar)
        #expect(archived.map(\.lead.title) == ["Done"])
        #expect(archived[0].children.map(\.title) == ["Done child"])
        #expect(lists.archivedToday(matching: "nothing", now: today, calendar: calendar).isEmpty)
        // Archived yesterday: gone from the list.
        #expect(lists.archivedToday(now: today.addingTimeInterval(86400), calendar: calendar).isEmpty)
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
        #expect(lists.board(matching: "").map(\.lead.title).sorted() == ["Fix the deal pages", "Run the tests"])
        // A child's branch finds its whole tree.
        #expect(lists.board(matching: "CHECKOUT").map(\.lead.title) == ["Fix the deal pages"])
        #expect(lists.board(matching: "tests").map(\.lead.title) == ["Run the tests"])
        #expect(lists.board(matching: "nothing").isEmpty)
    }

    @Test func aProjectsSessionsAreTaskTreesWithTheirChildrenUnderTheTop() {
        var lead = Script("01A")
        var child = Script("01B")
        var grandchild = Script("01C")
        var other = Script("01D")
        var host = machine("host-a", name: "a", sessions: ["01A", "01B", "01C", "01D"])
        for index in host.sessions.indices { host.sessions[index].projectId = "github.com/acme/app" }
        let sessions = [
            lead.model([created(task: "Lead")]),
            child.model([created(task: "Child", parent: "01A")]),
            grandchild.model([created(task: "Grandchild", parent: "01B")]),
            other.model([created(task: "Other")]),
        ]
        let live = Lists(machines: [host], sessions: Dictionary(uniqueKeysWithValues: sessions.map { ($0.key, $0) }))
            .projects[0].live
        let trees = TaskTree.trees(live)
        #expect(trees.map(\.lead.title).sorted() == ["Lead", "Other"])
        let tree = trees.first { $0.lead.title == "Lead" }
        #expect(tree?.children.map(\.title) == ["Child", "Grandchild"])
        #expect(tree?.agentCount == 2)
        // A search for a grandchild keeps its whole tree, folded under its top.
        #expect(trees.filter { $0.matches("grandchild") }.map(\.lead.title) == ["Lead"])
    }

    // MARK: Three states

    @Test func everySessionStateReducesToOneOfThreeTaskStates() {
        #expect(TaskState(.running) == .working)
        #expect(TaskState(.waiting) == .working)
        #expect(TaskState(.needsYou) == .waitingForYou)
        #expect(TaskState(.error) == .waitingForYou)
        #expect(TaskState(.idle) == .idle)
        #expect(TaskState(.done) == .idle)
        #expect(TaskState(.archived) == .idle)
        #expect(TaskState(.moved) == .idle)
    }

    @Test func aTaskWaitsForYouWhenAnySessionInItDoesAndSaysWhatFor() {
        var lead = Script("01A")
        var asking = Script("01B")
        var running = Script("01C")
        let waiting = board([
            lead.model([created(task: "Lead"), .sessionStatusChanged(status: .running, retryAt: nil)]),
            asking.model([
                created(task: "Asking", parent: "01A"),
                .approvalRequested(approvalId: "a", turnId: "t", toolCallId: "i", summary: "Run cargo test?",
                                   routedTo: .user, reason: nil),
            ]),
            running.model([created(task: "Running", parent: "01A"), .sessionStatusChanged(status: .running, retryAt: nil)]),
        ])[0]
        #expect(waiting.state == .waitingForYou)
        #expect(waiting.activity == "Run cargo test?")

        var top = Script("01D")
        var child = Script("01E")
        let working = board([
            top.model([created(task: "Top")]),
            child.model([created(task: "Child", parent: "01D"), .sessionStatusChanged(status: .running, retryAt: nil)]),
        ])[0]
        #expect(working.state == .working)

        var alone = Script("01F")
        #expect(board([alone.model([created(task: "Alone")])])[0].state == .idle)
    }

    @Test func aTaskIgnoresItsPullRequests() {
        var failing = Script("01A")
        let tree = board([
            failing.model([created(task: "Failing CI"), .prLinked(pr: pr(1, ci: .failing, review: .changesRequested))]),
        ])[0]
        #expect(tree.state == .idle)
    }

    // MARK: Grouping and ordering

    private static let now = Date(timeIntervalSince1970: 1_767_268_800) // 2026-01-01 12:00 UTC
    private static let calendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = .gmt
        return calendar
    }()

    /// A task whose top is `id`, last active `hoursAgo` before `now`.
    private func task(_ id: String, _ title: String, hoursAgo: Double?, project: String = "app") -> TaskTree {
        let lead = SessionSummary(
            key: SessionKey(hostId: "h", sessionId: id), title: title, project: project, branch: "", machine: "m",
            state: .idle, activity: "", age: "",
            updatedAt: hoursAgo.map { Self.now.addingTimeInterval(-$0 * 3600) })
        var tree = TaskTree(lead: lead, children: [])
        tree.project = project
        return tree
    }

    @Test func recentGroupsTasksByDayOfLastActivityNewestFirst() {
        let trees = [
            task("01A", "Two days ago", hoursAgo: 50),
            task("01B", "This morning", hoursAgo: 3),
            task("01C", "Not loaded", hoursAgo: nil),
            task("01D", "Last night", hoursAgo: 14),
            task("01E", "Just now", hoursAgo: 0.1),
            task("01F", "Also two days ago", hoursAgo: 49),
        ]
        let sections = BoardSection.sections(trees, by: .recent, now: Self.now, calendar: Self.calendar)
        #expect(sections.map(\.title).prefix(2) == ["Today", "Yesterday"])
        #expect(sections.last?.title == "Earlier")
        #expect(sections.map { $0.trees.map(\.lead.title) } == [
            ["Just now", "This morning"], ["Last night"], ["Also two days ago", "Two days ago"], ["Not loaded"],
        ])
        #expect(sections[2].title.contains("30"))
    }

    @Test func createdOrdersByTheTopSessionsId() {
        // ULIDs: the first was made a day before the second.
        let older = task("01KDV2Q8000000000000000000", "Older", hoursAgo: 0.1)
        let newer = task("01KDXN4C000000000000000000", "Newer", hoursAgo: 30)
        let sections = BoardSection.sections([older, newer], by: .created, now: Self.now, calendar: Self.calendar)
        #expect(sections.flatMap(\.trees).map(\.lead.title) == ["Newer", "Older"])
        #expect(sections.count == 2)
    }

    @Test func projectGroupsTasksByProjectEachNewestFirst() {
        let trees = [
            task("01A", "Herder old", hoursAgo: 5, project: "herder"),
            task("01B", "Trip", hoursAgo: 1, project: "trip1"),
            task("01C", "Herder new", hoursAgo: 2, project: "herder"),
            task("01D", "Loose", hoursAgo: 1, project: ""),
        ]
        let sections = BoardSection.sections(trees, by: .project, now: Self.now, calendar: Self.calendar)
        #expect(sections.map(\.title) == ["herder", "No project yet", "trip1"])
        #expect(sections[0].trees.map(\.lead.title) == ["Herder new", "Herder old"])
    }

    @Test func nameSortsTasksByTitleInOneUntitledSection() {
        let trees = [
            task("01A", "beta", hoursAgo: 1),
            task("01B", "Alpha", hoursAgo: 50),
            task("01C", "Task 10", hoursAgo: 2),
            task("01D", "Task 9", hoursAgo: nil),
        ]
        let sections = BoardSection.sections(trees, by: .name, now: Self.now, calendar: Self.calendar)
        #expect(sections.map(\.title) == [""])
        #expect(sections[0].trees.map(\.lead.title) == ["Alpha", "beta", "Task 9", "Task 10"])
        #expect(BoardSection.sections([], by: .name, now: Self.now).isEmpty)
    }

    @Test func theBoardAndSessionListsOfferTheirOwnSorts() {
        #expect(BoardSort.board.map(\.title) == ["Recent", "Created", "Project"])
        #expect(BoardSort.sessions.map(\.title) == ["Recent", "Created", "Name"])
        #expect(BoardSort.boardKey != BoardSort.sessionsKey)
    }

    // MARK: Project lists and Chats

    @Test func aProjectListNamesTheMachineOnlyWhenItsSessionsSpanMachines() {
        var onA = Script("01A", host: "host-a")
        var alsoOnA = Script("01B", host: "host-a")
        var onB = Script("01C", host: "host-b")
        var hostA = machine("host-a", name: "alpha", sessions: ["01A", "01B"])
        var hostB = machine("host-b", name: "beta", sessions: ["01C"])
        for index in hostA.sessions.indices { hostA.sessions[index].projectId = "github.com/acme/app" }
        hostB.sessions[0].projectId = "github.com/acme/app"
        let sessions = [
            onA.key: onA.model([created(task: "One", branch: "p1-one")]),
            alsoOnA.key: alsoOnA.model([created(task: "Two", branch: "")]),
            onB.key: onB.model([created(task: "Three", branch: "p1-three")]),
        ]
        let spanning = Lists(machines: [hostA, hostB], sessions: sessions).projects[0]
        #expect(ProjectSessions.spansMachines(spanning.sessions))
        let three = spanning.sessions.first { $0.title == "Three" }
        #expect(three.map { ProjectSessions.context($0, machines: true) } == "beta · p1-three")

        let single = Lists(machines: [hostA], sessions: sessions).projects[0]
        #expect(!ProjectSessions.spansMachines(single.sessions))
        let lines = single.sessions.map { ProjectSessions.context($0, machines: false) }
        #expect(lines.sorted() == ["", "p1-one"])
    }

    @Test func aProjectsHeaderSaysWhatWaitsAndWorksAndHowManyTasks() {
        var waiting = Script("01A")
        var working = Script("01B")
        var child = Script("01C")
        var idle = Script("01D")
        var archived = Script("01E")
        let project = projectLists([
            waiting.model([
                created(task: "Waiting"),
                .approvalRequested(approvalId: "a", turnId: "t", toolCallId: "i", summary: "Run cargo test?",
                                   routedTo: .user, reason: nil),
            ]),
            working.model([created(task: "Working"), .sessionStatusChanged(status: .running, retryAt: nil)]),
            child.model([created(task: "Child", parent: "01B"), .sessionStatusChanged(status: .running, retryAt: nil)]),
            idle.model([created(task: "Idle")]),
            archived.model([created(task: "Archived"), .sessionStatusChanged(status: .archived, retryAt: nil)]),
        ]).projects[0]
        // A child is no task of its own; an archived one counts.
        #expect(ProjectSessions.status(live: project.live, archived: project.archived)
            == "1 waiting for you · 1 working · 4 sessions")
        let quiet = projectLists([idle.model([created(task: "Idle")])]).projects[0]
        #expect(ProjectSessions.status(live: quiet.live, archived: quiet.archived) == "1 session")
    }

    @Test func aULIDsTimeIsItsFirstTenCharacters() {
        #expect(ULID.date("01ARZ3NDEKTSV4RRFFQ69G5FAV").map { ($0.timeIntervalSince1970 * 1000).rounded() } == 1_469_922_850_259)
        #expect(ULID.date("01A") == nil)
        #expect(ULID.date("0IARZ3NDEKTSV4RRFFQ69G5FAV") == nil)
    }

    @Test func aRowsLineSaysItsContextThenItsActivityAndHowManyAgents() {
        let lead = SessionSummary(
            key: SessionKey(hostId: "h", sessionId: "01A"), title: "Lead", project: "app", branch: "", machine: "m",
            state: .running, activity: "Editing Board.swift", age: "")
        let child = SessionSummary(
            key: SessionKey(hostId: "h", sessionId: "01B"), title: "Child", project: "app", branch: "", machine: "m",
            state: .idle, activity: "", age: "", depth: 1)
        let tree = TaskTree(lead: lead, children: [child])
        #expect(tree.line(context: "herder") == "herder · Editing Board.swift")
        #expect(tree.line(context: "") == "Editing Board.swift")
        #expect(tree.agentsLabel == "1 agent")
        #expect(TaskTree(lead: lead, children: [child, child]).agentsLabel == "2 agents")
    }

    @Test func unfoldedTasksAreRememberedOneALine() {
        let a = SessionKey(hostId: "h", sessionId: "01A")
        let b = SessionKey(hostId: "h2", sessionId: "01A")
        var tasks = UnfoldedTasks("")
        tasks.toggle(a)
        tasks.toggle(b)
        #expect(tasks.stored == "h/01A\nh2/01A")
        #expect(UnfoldedTasks(tasks.stored).contains(a))
        tasks.toggle(a)
        #expect(!tasks.contains(a))
        #expect(tasks.contains(b))
        #expect(tasks.stored == "h2/01A")
    }

    @Test func aProviderAgentWorksWhileItRuns() {
        #expect(TaskState(agent: .running) == .working)
        #expect(TaskState(agent: .ok) == .idle)
        #expect(TaskState(agent: .failed) == .idle)
    }
}
