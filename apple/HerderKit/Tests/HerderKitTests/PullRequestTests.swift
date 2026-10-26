import CoreGraphics
import Foundation
import Herder
@testable import HerderKit
import Testing

private func pr(_ number: UInt64, _ state: PrState) -> PullRequest {
    PullRequest(number: number, url: "https://github.com/acme/demo/pull/\(number)", title: "PR \(number)",
                headBranch: "b\(number)", state: state, ci: .passing, review: .none, mergeable: .clean)
}

private extension PRRollup {
    /// The rollup over `sessions`, each child found by its parent.
    init(of key: SessionKey, sessions: [SessionKey: SessionModel]) {
        let children = Dictionary(grouping: sessions.values.compactMap { model in
            model.parent.map { (parent: SessionKey(hostId: model.key.hostId, sessionId: $0), child: model.key) }
        }, by: \.parent).mapValues { $0.map(\.child) }
        self.init(of: key, session: { sessions[$0] }, children: children)
    }
}

struct PRRollupTests {
    private func session(_ id: String, task: String, parent: SessionId? = nil, prs: [PullRequest]) -> (SessionKey, SessionModel) {
        var script = Script(id)
        let model = script.model([created(task: task, parent: parent)] + prs.map { .prLinked(pr: $0) })
        return (script.key, model)
    }

    /// A primary with one PR, two children (one with a grandchild) and an unrelated session.
    private var tree: [SessionKey: SessionModel] {
        Dictionary(uniqueKeysWithValues: [
            session("01P", task: "Primary", prs: [pr(181, .open)]),
            session("01C", task: "Child b", parent: "01P", prs: [pr(190, .merged), pr(191, .open)]),
            session("01G", task: "Grandchild", parent: "01C", prs: [pr(200, .closed), pr(201, .draft)]),
            session("01D", task: "Child a", parent: "01P", prs: [pr(181, .open), pr(185, .merged)]),
            session("01X", task: "Elsewhere", prs: [pr(300, .open)]),
        ])
    }

    @Test func theRollupHoldsOwnAndNestedDescendantsPRsOnce() {
        let rollup = PRRollup(of: SessionKey(hostId: "host-a", sessionId: "01P"), sessions: tree)
        // The primary's, then Child a's, Child b's and the grandchild's. #181 is linked to the
        // primary and to Child a: it shows under the primary only.
        #expect(rollup.all.map(\.number) == [181, 185, 191, 190, 201, 200])
        // A child's own roll-up leaves its parent and siblings out.
        let child = PRRollup(of: SessionKey(hostId: "host-a", sessionId: "01C"), sessions: tree)
        #expect(child.all.map(\.number) == [191, 190, 201, 200])
    }

    @Test func eachSessionsPRsSortOpenDraftMergedClosed() {
        let (key, model) = session("01P", task: "Primary",
                                   prs: [pr(1, .closed), pr(2, .merged), pr(3, .draft), pr(4, .open), pr(5, .open)])
        #expect(PRRollup(of: key, sessions: [key: model]).all.map(\.number) == [5, 4, 3, 2, 1])
    }

    @Test func theChipSaysHowManyAreOpenAndItsTooltipTheWholeCount() {
        let rollup = PRRollup(of: SessionKey(hostId: "host-a", sessionId: "01P"), sessions: tree)
        #expect(rollup.chip == "3 open")
        #expect(rollup.summary == "6 pull requests · 3 open")
        let (key, model) = session("01P", task: "Primary", prs: [pr(181, .merged)])
        let single = PRRollup(of: key, sessions: [key: model])
        #expect(single.chip == "#181")
        #expect(single.summary == "Pull request #181, merged")
        let (done, finished) = session("01P", task: "Primary", prs: [pr(1, .merged), pr(2, .closed)])
        #expect(PRRollup(of: done, sessions: [done: finished]).chip == "2 PRs")
        #expect(PRRollup(of: done, sessions: [done: finished]).summary == "2 pull requests")
    }

    @Test func theChipIsTintedByTheStateOfThePRs() {
        func urgency(_ prs: [PullRequest]) -> Urgency {
            let (key, model) = session("01P", task: "Primary", prs: prs)
            return PRRollup(of: key, sessions: [key: model]).urgency
        }
        var failing = pr(3, .open)
        failing.ci = .failing
        var conflicting = pr(4, .draft)
        conflicting.mergeable = .conflicting
        #expect(urgency([pr(1, .merged), failing]) == .failing)
        #expect(urgency([conflicting]) == .failing)
        #expect(urgency([pr(1, .merged), pr(2, .draft)]) == .running)
        #expect(urgency([pr(1, .merged), pr(2, .closed)]) == .done)
        #expect(urgency([pr(2, .closed)]) == .quiet)
        // A failing check on a merged PR is history: it no longer tints the chip.
        var merged = pr(5, .merged)
        merged.ci = .failing
        #expect(urgency([merged]) == .done)
    }
}

struct SessionColumnTests {
    @Test func theColumnStartsAtTheTranscriptAndEndsAtTheComposer() {
        // A wide pane centres both: the transcript's 760 points, the composer's 784.
        let wide = SessionColumn.insets(in: 1060)
        #expect(wide.leading == 150 && wide.trailing == 138)
        // A narrow one keeps their margins.
        let narrow = SessionColumn.insets(in: 700)
        #expect(narrow.leading == 16 && narrow.trailing == 12)
        // In between, the transcript is centred while the composer still keeps its margin.
        let between = SessionColumn.insets(in: 800)
        #expect(between.leading == 20 && between.trailing == 12)
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
            .itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: "c", turnId: "t1", body: .toolCall(name: "Bash", input: "{}"))),
            .approvalRequested(approvalId: "a", turnId: "t1", toolCallId: "c", summary: "Bash: ls", routedTo: .user, reason: nil),
            .approvalResolved(approvalId: "a", decision: .allow, answeredBy: .user),
            .turnCompleted(turnId: "t1", usage: nil),
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
                Account(accountId: "gpt", provider: "codex", label: "gpt", configDir: nil, email: nil, usage: []),
                Account(accountId: "main", provider: "claude", label: "main", configDir: nil, email: nil, usage: []),
            ]
            fleet.setMachinesForTesting([host])
            #expect(fleet.defaultProvider(on: "h", projectId: nil) == "claude")
        }
    }
}

struct RoundTwoTests {
    @Test func aProjectWithoutSessionsStillLists() {
        let project = Project(projectId: "github.com/acme/new", name: "new", paths: ["/src/new"], defaultPermissionMode: nil,
                              defaultAccount: nil, setupCommand: nil)
        let lists = Lists(machines: [machine("h", name: "alpha", sessions: [], projects: [project])], sessions: [:])
        #expect(lists.projects.map(\.name) == ["new"])
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

struct DraftTests {
    @Test func aDraftInAnAddedProjectNamesTheProjectOnly() {
        let draft = Draft(hostId: "h", projectId: "github.com/acme/homelab", repo: "/home/me/homelab")
        #expect(draft.createArguments.projectId == "github.com/acme/homelab")
        #expect(draft.createArguments.repo == nil)
        let path = Draft(hostId: "h", projectId: nil, repo: "/home/me/other")
        #expect(path.createArguments.repo == "/home/me/other")
        #expect(path.createArguments.projectId == nil)
    }

    @Test func aNewSessionStartsInTheProjectInView() {
        var host = machine("host-a", name: "a", sessions: ["01A", "01B"])
        host.sessions[0].projectId = "github.com/acme/app"
        var app = Script("01A")
        var loose = Script("01B")
        let lists = Lists(machines: [host], sessions: [app.key: app.model([created()]), loose.key: loose.model([created()])])
        #expect(lists.project(for: .project("github.com/acme/app")) == "github.com/acme/app")
        // Anywhere but a project pane leaves it to the picker.
        #expect(lists.project(for: .board) == nil)
        #expect(lists.project(for: .project("")) == nil)
    }
}

struct FolderTypingTests {
    @Test func aTypedPathSplitsIntoTheFolderAndTheNameBeingTyped() {
        #expect(FolderBrowser.split("~") == ("~", ""))
        #expect(FolderBrowser.split("~/Proj") == ("~", "Proj"))
        #expect(FolderBrowser.split("/home/me/Projects/") == ("/home/me/Projects", ""))
        #expect(FolderBrowser.split("/ho") == ("/", "ho"))
    }
}

struct LinkVerdictTests {
    private func quality(average: UInt32?, min: UInt32? = nil, max: UInt32? = nil, missed: UInt32 = 0) -> ConnectionQuality {
        ConnectionQuality(connectedSince: nil, reconnects: 0, lastRttMs: average, averageRttMs: average,
                          minRttMs: min ?? average, maxRttMs: max ?? average, missedPongs: missed)
    }

    @Test func aLinkIsJudgedByItsRoundTrips() {
        #expect(LinkVerdict(quality(average: nil)) == nil)
        #expect(LinkVerdict(quality(average: 30))?.level == .good)
        #expect(LinkVerdict(quality(average: 150))?.level == .fair)
        #expect(LinkVerdict(quality(average: 80, min: 20, max: 400))?.summary == "Unsteady: 20–400 ms")
        #expect(LinkVerdict(quality(average: 450))?.level == .poor)
        #expect(LinkVerdict(quality(average: 30, missed: 2))?.summary == "2 pings went unanswered")
    }
}

struct ProjectIconLookupTests {
    /// An icon picture `width` units wide and one high, drawn `ProjectIconImage.side` wide, so
    /// `side / width` high.
    private func picture(width: Int) -> CGImage {
        ProjectIconImage.picture(Data(#"<svg xmlns="http://www.w3.org/2000/svg" width="\#(width)" height="1"/>"#.utf8))!
    }

    @Test func aProjectShowsTheIconAnyMachineListedAndThisAppFetched() {
        let bare = Project(projectId: "github.com/acme/app", name: "app", paths: [], defaultPermissionMode: nil,
                           defaultAccount: nil, setupCommand: nil, icon: nil)
        var iconed = bare
        iconed.icon = "abc"
        iconed.iconBackground = "#ffffff"
        var backed = bare
        backed.iconBackground = "#000000"
        let machines = [machine("a", name: "alpha", sessions: [], projects: [backed]),
                        machine("b", name: "beta", sessions: [], projects: [iconed])]
        // Without a fetched icon, the initial shows on a machine's background.
        #expect(Fleet.icon(of: "github.com/acme/app", on: machines, fetched: [:])
                == ProjectIconImage(picture: nil, background: "#000000"))
        #expect(Fleet.icon(of: "github.com/acme/app", on: [machine("a", name: "alpha", sessions: [], projects: [bare])],
                           fetched: [:]) == nil)
        // The background comes from the machine whose icon is shown.
        let picture = picture(width: 1)
        #expect(Fleet.icon(of: "github.com/acme/app", on: machines, fetched: ["abc": picture])
                == ProjectIconImage(picture: picture, background: "#ffffff"))
        #expect(Fleet.icon(of: nil, on: machines, fetched: ["abc": picture]) == nil)
    }

    @Test func theInitialIsDarkOnALightBackgroundAndWhiteOnADarkOne() {
        #expect(ProjectIcon.isLight(0xFFFFFF))
        #expect(ProjectIcon.isLight(0xE5E5E5))
        #expect(!ProjectIcon.isLight(0x2A2A2A))
        #expect(!ProjectIcon.isLight(0x000000))
    }

    @Test func everyDeviceShowsTheSameIconWhateverOrderItPairedTheMachinesIn() {
        let found = Project(projectId: "github.com/acme/app", name: "app", paths: [], defaultPermissionMode: nil,
                            defaultAccount: nil, setupCommand: nil, icon: "found")
        var uploaded = found
        uploaded.icon = "up"
        uploaded.iconUploaded = true
        var other = found
        other.icon = "other"
        // Told apart by their heights.
        let fetched = ["found": picture(width: 1), "up": picture(width: 2), "other": picture(width: 3)]
        let a = machine("a", name: "alpha", sessions: [], projects: [found])
        let b = machine("b", name: "beta", sessions: [], projects: [uploaded])
        let c = machine("c", name: "gamma", sessions: [], projects: [other])
        // An uploaded icon wins over those found in clones, in either order.
        #expect(Fleet.icon(of: "github.com/acme/app", on: [a, b], fetched: fetched)?.picture?.height == 128)
        #expect(Fleet.icon(of: "github.com/acme/app", on: [b, a], fetched: fetched)?.picture?.height == 128)
        // Between found icons, the machine with the lowest id wins.
        #expect(Fleet.icon(of: "github.com/acme/app", on: [c, a], fetched: fetched)?.picture?.height == 256)
        #expect(Fleet.icon(of: "github.com/acme/app", on: [a, c], fetched: fetched)?.picture?.height == 256)
    }

    @Test func everyDeviceShowsTheSameNameWhateverOrderItPairedTheMachinesIn() {
        let plain = Project(projectId: "github.com/acme/app", name: "app", paths: [], defaultPermissionMode: nil,
                            defaultAccount: nil, setupCommand: nil, icon: nil)
        var renamed = plain
        renamed.name = "Acme"
        var other = plain
        other.name = "Other"
        let a = machine("a", name: "alpha", sessions: [], projects: [plain])
        let b = machine("b", name: "beta", sessions: [], projects: [renamed])
        let c = machine("c", name: "gamma", sessions: [], projects: [other])
        // A name an owner gave wins over the repository's, in either order.
        #expect(Lists.projectName("github.com/acme/app", machines: [a, b]) == "Acme")
        #expect(Lists.projectName("github.com/acme/app", machines: [b, a]) == "Acme")
        // Between given names, the machine with the lowest id wins.
        #expect(Lists.projectName("github.com/acme/app", machines: [c, b]) == "Acme")
        #expect(Lists.projectName("github.com/acme/app", machines: [a]) == "app")
    }

    @Test func anIconBackgroundIsAColourOnlyAsRrggbb() {
        #expect(ProjectIcon.colour("#ffffff") != nil)
        #expect(ProjectIcon.colour("#1A2b3C") != nil)
        #expect(ProjectIcon.colour("ffffff") == nil)
        #expect(ProjectIcon.colour("#fff") == nil)
        #expect(ProjectIcon.colour("#gggggg") == nil)
    }
}

struct CompactTabTests {
    @Test func compactWidthReachesEverySidebarSection() {
        #expect(CompactTab.allCases.map(\.title) == ["Board", "Chats", "Projects", "Machines"])
        #expect(CompactTab(.project("p")) == .projects)
        #expect(CompactTab(.chats) == .chats)
        #expect(CompactTab(.board) == .board)
        #expect(CompactTab(.skills) == .machines)
        // Usage shows on Providers, inside Machines.
        #expect(CompactTab(.providers) == .machines)
        #expect(CompactTab(.machines) == .machines)
    }
}
