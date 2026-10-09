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

    @Test func aSessionsPRsAreASheetOnCompactWidthAsThePopoverIsWiderThanAPhone() {
        #expect(PRListPresentation(compact: true) == .sheet)
        #expect(PRListPresentation(compact: false) == .popover)
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
        #expect(rollup.groups.map(\.title) == ["Primary", "Child a", "Child b", "Grandchild"])
        #expect(rollup.groups.map(\.depth) == [0, 1, 1, 2])
        // #181 is linked to the primary and to Child a: it shows under the primary only.
        #expect(rollup.groups.map { $0.prs.map(\.number) } == [[181], [185], [191, 190], [201, 200]])
        #expect(rollup.all.count == 6)
        // A child's own roll-up leaves its parent and siblings out.
        let child = PRRollup(of: SessionKey(hostId: "host-a", sessionId: "01C"), sessions: tree)
        #expect(child.groups.map(\.title) == ["Child b", "Grandchild"])
    }

    @Test func eachGroupSortsOpenDraftMergedClosed() {
        let (key, model) = session("01P", task: "Primary",
                                   prs: [pr(1, .closed), pr(2, .merged), pr(3, .draft), pr(4, .open), pr(5, .open)])
        let rollup = PRRollup(of: key, sessions: [key: model])
        #expect(rollup.groups[0].prs.map(\.number) == [5, 4, 3, 2, 1])
        #expect(rollup.groups[0].finished.map(\.number) == [2, 1])
        #expect(PRRollup.folded(rollup.groups[0].finished) == "1 merged · 1 closed")
        #expect(PRRollup.folded([pr(7, .merged), pr(8, .merged)]) == "2 merged")
    }

    @Test func theChipSaysTheCountAndHowManyAreOpen() {
        let rollup = PRRollup(of: SessionKey(hostId: "host-a", sessionId: "01P"), sessions: tree)
        #expect(rollup.chip == "6 PRs · 3 open")
        #expect(rollup.shortChip == "3/6")
        #expect(rollup.urgent == .open)
        let (key, model) = session("01P", task: "Primary", prs: [pr(181, .merged)])
        let single = PRRollup(of: key, sessions: [key: model])
        #expect(single.chip == "#181")
        #expect(single.shortChip == "#181")
        #expect(single.urgent == .merged)
        let (done, finished) = session("01P", task: "Primary", prs: [pr(1, .merged), pr(2, .closed)])
        #expect(PRRollup(of: done, sessions: [done: finished]).chip == "2 PRs")
        #expect(PRRollup(of: done, sessions: [done: finished]).shortChip == "2")
    }

    @Test func theListFiltersToOpenAndSearchesNumberTitleAndBranch() {
        let rollup = PRRollup(of: SessionKey(hostId: "host-a", sessionId: "01P"), sessions: tree)
        let open = rollup.shown(openOnly: true, query: "")
        #expect(open.map(\.title) == ["Primary", "Child b", "Grandchild"])
        #expect(open.flatMap { $0.prs.map(\.number) } == [181, 191, 201])
        #expect(rollup.shown(openOnly: false, query: "#19").flatMap { $0.prs.map(\.number) } == [191, 190])
        #expect(rollup.shown(openOnly: false, query: "PR 200").flatMap { $0.prs.map(\.number) } == [200])
        #expect(rollup.shown(openOnly: false, query: "b185").map(\.title) == ["Child a"])
        #expect(rollup.shown(openOnly: true, query: "nothing").isEmpty)
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

struct DraftTests {
    @Test func aDraftInAnAddedProjectNamesTheProjectOnly() {
        let draft = Draft(hostId: "h", projectId: "github.com/acme/homelab", repo: "/home/me/homelab")
        #expect(draft.createArguments.projectId == "github.com/acme/homelab")
        #expect(draft.createArguments.repo == nil)
        let path = Draft(hostId: "h", projectId: nil, repo: "/home/me/other")
        #expect(path.createArguments.repo == "/home/me/other")
        #expect(path.createArguments.projectId == nil)
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

struct RemoveProjectTests {
    @Test func onlySessionsThatAreNotArchivedBlockRemovingAProject() {
        var host = machine("h", name: "alpha", sessions: ["01A", "01B", "01C"])
        host.sessions[0].projectId = "github.com/acme/app"
        host.sessions[1].projectId = "github.com/acme/app"
        host.sessions[1].status = .archived
        host.sessions[2].projectId = "github.com/acme/other"
        #expect(ProjectSettingsForm.liveSessions(of: "github.com/acme/app", on: host) == 1)
        host.sessions[0].status = .archived
        #expect(ProjectSettingsForm.liveSessions(of: "github.com/acme/app", on: host) == 0)
    }
}

struct ProjectIconLookupTests {
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
                == ProjectIconImage(data: nil, background: "#000000"))
        #expect(Fleet.icon(of: "github.com/acme/app", on: [machine("a", name: "alpha", sessions: [], projects: [bare])],
                           fetched: [:]) == nil)
        // The background comes from the machine whose icon is shown.
        #expect(Fleet.icon(of: "github.com/acme/app", on: machines, fetched: ["abc": Data([1])])
                == ProjectIconImage(data: Data([1]), background: "#ffffff"))
        #expect(Fleet.icon(of: nil, on: machines, fetched: ["abc": Data([1])]) == nil)
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
        let fetched = ["found": Data([1]), "up": Data([2]), "other": Data([3])]
        let a = machine("a", name: "alpha", sessions: [], projects: [found])
        let b = machine("b", name: "beta", sessions: [], projects: [uploaded])
        let c = machine("c", name: "gamma", sessions: [], projects: [other])
        // An uploaded icon wins over those found in clones, in either order.
        #expect(Fleet.icon(of: "github.com/acme/app", on: [a, b], fetched: fetched)?.data == Data([2]))
        #expect(Fleet.icon(of: "github.com/acme/app", on: [b, a], fetched: fetched)?.data == Data([2]))
        // Between found icons, the machine with the lowest id wins.
        #expect(Fleet.icon(of: "github.com/acme/app", on: [c, a], fetched: fetched)?.data == Data([1]))
        #expect(Fleet.icon(of: "github.com/acme/app", on: [a, c], fetched: fetched)?.data == Data([1]))
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
        #expect(CompactTab.allCases.map(\.title) == ["Board", "Projects", "Usage", "Machines"])
        #expect(CompactTab(.project("p")) == .projects)
        #expect(CompactTab(.board) == .board)
        // Pull Requests has no tab of its own: it opens from the Board.
        #expect(CompactTab(.pullRequests) == .board)
        #expect(CompactTab(.usage) == .usage)
        #expect(CompactTab(.skills) == .machines)
        #expect(CompactTab(.machines) == .machines)
        // A vault has no tab of its own: it shows inside Machines.
        #expect(CompactTab(.vault) == .machines)
    }
}
