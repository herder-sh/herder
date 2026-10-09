import Foundation
import Herder
@testable import HerderKit
import Testing

func machine(
    _ hostId: HostId, name: String, sessions: [SessionId], projects: [Project] = [], hosts: [FleetHost] = []
) -> Machine {
    Machine(
        hostId: hostId, name: name, addresses: [], fingerprint: "", connection: .connected,
        quality: ConnectionQuality(connectedSince: nil, reconnects: 0, lastRttMs: nil, averageRttMs: nil, minRttMs: nil,
                                   maxRttMs: nil, missedPongs: 0),
        role: .owner,
        sessions: sessions.map {
            SessionHead(sessionId: $0, hostId: nil, headSeq: 0, status: .idle, parent: nil, parentHost: nil, task: nil,
                        title: nil, projectId: nil, accountId: "main", childrenNeedYou: 0, queue: [])
        },
        hosts: hosts, projects: projects, accounts: [], failover: FailoverSettings(pin: false), terminals: [],
        resources: nil, sessionUsage: [:], vault: nil)
}

struct ListsTests {
    @Test func requestsAreNewestFirst() {
        var older = Script("01A")
        var newer = Script("01B")
        // `newer` asks later: its events are stamped after two of `older`'s.
        _ = newer.event(.turnStarted(turnId: "x"))
        _ = newer.event(.turnStarted(turnId: "x"))
        let sessions = [
            older.key: older.model([
                created(), .approvalRequested(approvalId: "a", turnId: "t", toolCallId: "i", summary: "old", routedTo: .user, reason: nil),
            ]),
            newer.key: newer.model([
                created(), .approvalRequested(approvalId: "b", turnId: "t", toolCallId: "i", summary: "new", routedTo: .user, reason: nil),
            ]),
        ]
        let lists = Lists(machines: [machine("host-a", name: "a", sessions: ["01A", "01B"])], sessions: sessions)
        #expect(lists.requests.map(\.requestId) == ["b", "a"])
    }

    @Test func childrenFollowTheirParentOldestFirst() {
        var parent = Script("01A")
        var older = Script("01B")
        var younger = Script("01C")
        var other = Script("01D")
        let sessions = [
            parent.key: parent.model([created(task: "Lead")]),
            older.key: older.model([created(task: "First", parent: "01A")]),
            younger.key: younger.model([
                created(task: "Second", parent: "01A"),
                .approvalRequested(approvalId: "a", turnId: "t", toolCallId: "i", summary: "s", routedTo: .user, reason: nil),
            ]),
            other.key: other.model([created(task: "Solo")]),
        ]
        let lists = Lists(
            machines: [machine("host-a", name: "a", sessions: ["01A", "01B", "01C", "01D"])], sessions: sessions)
        let rows = lists.projects.flatMap(\.sessions)
        #expect(rows.map(\.title) == ["Solo", "Lead", "First", "Second"])
        #expect(rows.map(\.depth) == [0, 0, 1, 1])
        #expect(rows[1].children == 2)
        #expect(rows[1].childrenNeedYou == 1)
    }

    @Test func projectsAreNamedByTheirMachineAndSpanMachines() {
        var onA = Script("01A", host: "host-a")
        var onB = Script("01B", host: "host-b")
        let project = Project(projectId: "github.com/acme/demo", name: "Demo", paths: [], defaultPermissionMode: nil, defaultAccount: nil, setupCommand: nil)
        let machines = [
            machine("host-a", name: "alpha", sessions: ["01A"], projects: [project]),
            machine("host-b", name: "beta", sessions: ["01B"]),
        ]
        var withProject = machines
        withProject[0].sessions[0].projectId = "github.com/acme/demo"
        withProject[1].sessions[0].projectId = "github.com/acme/demo"
        let lists = Lists(machines: withProject, sessions: [
            onA.key: onA.model([created()]), onB.key: onB.model([created()]),
        ])
        #expect(lists.projects.map(\.name) == ["Demo"])
        #expect(lists.projects[0].machines == ["alpha", "beta"])
        // Rows carry the project's id, so they can show its icon.
        #expect(lists.projects[0].sessions.map(\.projectId) == ["github.com/acme/demo", "github.com/acme/demo"])
    }

    @Test func taskTreesStayWholeAndGoToArchivedOnlyWhenAllOfThemIs() {
        let archive = EventBody.sessionStatusChanged(status: .archived, retryAt: nil)
        var scripts = ["01A", "01B", "01C", "01D", "01E", "01F", "01G"].map { Script($0) }
        let sessions = [
            scripts[0].model([created(task: "Live lead")]),
            scripts[1].model([created(task: "Archived child of a live lead", parent: "01A"), archive]),
            scripts[2].model([created(task: "Live child", parent: "01A")]),
            scripts[3].model([created(task: "Archived lead"), archive]),
            scripts[4].model([created(task: "Its archived child", parent: "01D"), archive]),
            scripts[5].model([created(task: "Archived lead of a live child"), archive]),
            scripts[6].model([created(task: "Live child of an archived lead", parent: "01F")]),
        ]
        var host = machine("host-a", name: "a", sessions: scripts.map(\.key.sessionId))
        for index in host.sessions.indices { host.sessions[index].projectId = "github.com/acme/app" }
        let lists = Lists(machines: [host], sessions: Dictionary(uniqueKeysWithValues: sessions.map { ($0.key, $0) }))
        let project = lists.projects[0]
        // An archived lead still leads its live child; an archived child stays under its live
        // lead, after the live children.
        #expect(project.live.map(\.title) == [
            "Archived lead of a live child", "Live child of an archived lead",
            "Live lead", "Live child", "Archived child of a live lead",
        ])
        #expect(project.live.map(\.depth) == [0, 1, 0, 1, 1])
        // Only a tree archived whole is in Archived, still a tree.
        #expect(project.archived.map(\.title) == ["Archived lead", "Its archived child"])
        #expect(project.archived.map(\.depth) == [0, 1])
        #expect(project.sessions.map(\.key) == (project.live + project.archived).map(\.key))
        // A lead counts all its children, archived or not.
        #expect(project.live[2].children == 2)
    }

    @Test func archivingAParentKeepsItsChildrenUnderIt() {
        var parent = Script("01A")
        var child = Script("01B")
        var host = machine("host-a", name: "a", sessions: ["01A", "01B"])
        for index in host.sessions.indices { host.sessions[index].projectId = "github.com/acme/app" }
        let lists = Lists(machines: [host], sessions: [
            parent.key: parent.model([created()]),
            child.key: child.model([created(parent: "01A"), .sessionStatusChanged(status: .running, retryAt: nil)]),
        ], archiving: [parent.key])
        #expect(lists.projects[0].live.map(\.key) == [parent.key, child.key])
        #expect(lists.projects[0].live.map(\.depth) == [0, 1])
        #expect(lists.projects[0].archived.isEmpty)
    }

    @Test func archivingAChildKeepsItUnderItsLiveParent() {
        var parent = Script("01A")
        var child = Script("01B")
        var host = machine("host-a", name: "a", sessions: ["01A", "01B"])
        for index in host.sessions.indices { host.sessions[index].projectId = "github.com/acme/app" }
        let lists = Lists(machines: [host], sessions: [
            parent.key: parent.model([created()]), child.key: child.model([created(parent: "01A")]),
        ], archiving: [child.key])
        #expect(lists.projects[0].live.map(\.key) == [parent.key, child.key])
        #expect(lists.projects[0].live.map(\.depth) == [0, 1])
        #expect(lists.projects[0].live[0].children == 1)
        #expect(lists.projects[0].archived.isEmpty)
    }

    @Test func aChildIsNestedByTheListsParentBeforeItsEventsLoad() {
        var parent = Script("01A")
        var host = machine("host-a", name: "a", sessions: ["01A", "01B"])
        host.sessions[1].parent = "01A"
        let lists = Lists(machines: [host], sessions: [parent.key: parent.model([created()])])
        #expect(lists.projects[0].sessions.map(\.key.sessionId) == ["01A", "01B"])
        #expect(lists.projects[0].sessions.map(\.depth) == [0, 1])
    }

    @Test func aSessionBeingArchivedIsListedArchivedAlready() {
        var archiving = Script("01A")
        var live = Script("01B")
        var host = machine("host-a", name: "a", sessions: ["01A", "01B"])
        for index in host.sessions.indices { host.sessions[index].projectId = "github.com/acme/app" }
        let lists = Lists(machines: [host], sessions: [
            archiving.key: archiving.model([created()]), live.key: live.model([created()]),
        ], archiving: [archiving.key])
        #expect(lists.projects[0].live.map(\.key) == [live.key])
        #expect(lists.projects[0].archived.map(\.key) == [archiving.key])
    }

    @Test func aRemovedProjectLeavesTheListWithItsArchivedSessions() {
        // The machine lists neither the project nor a project for its sessions: it was removed.
        var archived = Script("01A")
        var live = Script("01B")
        let lists = Lists(machines: [machine("host-a", name: "a", sessions: ["01A", "01B"])], sessions: [
            archived.key: archived.model([created(), .sessionStatusChanged(status: .archived, retryAt: nil)]),
            live.key: live.model([created()]),
        ])
        // The live session waits for a project; the archived one keeps none in the list.
        #expect(lists.projects.map(\.projectId) == [nil])
        #expect(lists.projects[0].sessions.map(\.key) == [live.key])

        let withoutLive = Lists(machines: [machine("host-a", name: "a", sessions: ["01A"])], sessions: [
            archived.key: archived.model([created(), .sessionStatusChanged(status: .archived, retryAt: nil)]),
        ])
        #expect(withoutLive.projects.isEmpty)
    }

    @Test func aVaultSessionShowsItsHostAndWhetherItIsOffline() {
        var script = Script("01A", host: "vault")
        var vault = machine("vault", name: "vault", sessions: ["01A"], hosts: [
            FleetHost(hostId: "h1", hostName: "builder", online: false, lastSeen: "2026-01-01T00:00:00Z"),
        ])
        vault.sessions[0].hostId = "h1"
        let lists = Lists(machines: [vault], sessions: [script.key: script.model([created()])])
        let row = lists.projects.flatMap(\.sessions).first
        #expect(row?.machine == "builder")
        #expect(row?.machineOffline == true)
        #expect(lists.machines.first?.hosts.map(\.sessions) == [1])
    }

    /// Three projects on one machine: "Alpha" active first, "Beta" active last, "Idle" with no
    /// sessions, and a session no project holds yet.
    private func threeProjects() -> Lists {
        var alpha = Script("01A")
        var beta = Script("01B")
        var loose = Script("01C")
        var other = Script("01D")
        let projects = [
            Project(projectId: "github.com/acme/alpha", name: "Alpha", paths: ["/src/alpha"], defaultPermissionMode: nil,
                    defaultAccount: nil, setupCommand: nil),
            Project(projectId: "github.com/acme/beta", name: "Beta", paths: ["/src/beta-repo"], defaultPermissionMode: nil,
                    defaultAccount: nil, setupCommand: nil),
            Project(projectId: "github.com/acme/idle", name: "Idle", paths: ["/src/idle"], defaultPermissionMode: nil,
                    defaultAccount: nil, setupCommand: nil),
        ]
        var host = machine("host-a", name: "a", sessions: ["01A", "01B", "01C", "01D"], projects: projects)
        host.sessions[0].projectId = "github.com/acme/alpha"
        host.sessions[1].projectId = "github.com/acme/beta"
        host.sessions[3].projectId = "github.com/acme/alpha"
        // Beta's session has the latest event of all.
        return Lists(machines: [host], sessions: [
            alpha.key: alpha.model([created(task: "Fix the login page")]),
            beta.key: beta.model([created(task: "Tidy"), .turnStarted(turnId: "t"), .turnStarted(turnId: "u")]),
            loose.key: loose.model([created(task: "Waiting")]),
            other.key: other.model([created(task: "Write docs")]),
        ])
    }

    @Test func projectsListMostRecentlyActiveFirstWithTheUnassignedLast() {
        let lists = threeProjects()
        // The sidebar keeps its alphabetical order.
        #expect(lists.projects.map(\.name) == ["Alpha", "Beta", "Idle", "No project yet"])
        #expect(ProjectGroup.found(lists.projects, query: "").map(\.name) == ["Beta", "Alpha", "Idle", "No project yet"])
        #expect(lists.projects.first { $0.name == "Idle" }?.age == "")
        #expect(lists.projects.first { $0.name == "Beta" }?.paths == ["/src/beta-repo"])
    }

    @Test func theProjectPickerListsTheMostRecentlyUsedProjectFirst() {
        let groups = threeProjects().projects
        let rows: [ProjectPicker.Row] = [
            ("github.com/acme/alpha", "Alpha", []), ("github.com/acme/idle", "Idle", []),
            ("github.com/acme/new", "New", []), ("github.com/acme/beta", "Beta", []),
        ]
        #expect(ProjectPicker.ranked(rows, by: groups).map(\.name) == ["Beta", "Alpha", "Idle", "New"])
    }

    @Test func aSearchFindsProjectsByNameOrPathAndByTheirSessions() {
        let projects = threeProjects().projects
        func found(_ query: String) -> [String] { ProjectGroup.found(projects, query: query).map(\.name) }
        #expect(found("ALPHA") == ["Alpha"])
        #expect(found("  beta-repo ") == ["Beta"])
        #expect(found("acme/idle") == ["Idle"])
        #expect(found("nothing like it").isEmpty)
        // A project found by its name keeps all its sessions.
        #expect(ProjectGroup.found(projects, query: "alpha").first?.live.map(\.title) == ["Write docs", "Fix the login page"])
        // One found through a session keeps just the sessions that match.
        let login = ProjectGroup.found(projects, query: "login")
        #expect(login.map(\.name) == ["Alpha"])
        #expect(login.first?.live.map(\.title) == ["Fix the login page"])
        #expect(login.first?.matches("login") == false)
    }

    @Test func manySessionsBuildQuickly() {
        // The lists are rebuilt as sessions stream; finding children over every session for
        // each one took seconds with a few thousand and froze the app on launch.
        let ids = (0..<3000).map { String(format: "01%05d", $0) }
        var sessions: [SessionKey: SessionModel] = [:]
        for id in ids {
            var script = Script(id)
            sessions[script.key] = script.model([created(task: id, parent: id == ids[0] ? nil : ids[0])])
        }
        let machines = [machine("host-a", name: "a", sessions: ids)]
        let start = ContinuousClock.now
        let lists = Lists(machines: machines, sessions: sessions)
        #expect(ContinuousClock.now - start < .seconds(1))
        #expect(lists.projects[0].live.first?.children == 2999)
        #expect(lists.projects.flatMap(\.sessions).count == 3000)
    }

    @Test func usageWindowsAreLabelledAsInTheTUI() {
        #expect(Lists.usageLabel("five_hour") == "Session")
        #expect(Lists.usageLabel("seven_day") == "Weekly")
        #expect(Lists.usageLabel("seven_day_opus") == "Weekly · Opus")
        #expect(Lists.usageLabel("monthly_spend") == "Monthly spend")
    }
}

@MainActor
struct FleetListsTests {
    /// Whether `change` makes the fleet publish new lists.
    private func publishes(_ fleet: Fleet, _ change: () -> Void) -> Bool {
        let published = Published()
        withObservationTracking { _ = fleet.lists } onChange: { published.value = true }
        change()
        return published.value
    }

    private final class Published: @unchecked Sendable { var value = false }

    @Test func listsArePublishedOnlyWhenTheyChange() throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("the profile did not open")
            return
        }
        let machines = [machine("host-a", name: "a", sessions: ["01A"])]
        #expect(publishes(fleet) { fleet.setMachinesForTesting(machines) })
        #expect(!publishes(fleet) { fleet.setMachinesForTesting(machines) })
        #expect(fleet.lists.projects.flatMap(\.sessions).map(\.key.sessionId) == ["01A"])
    }

    /// `devbox` runs `01A` and `01B`; the vault replicates both, and `01C` of a host not paired here.
    @Test func aSessionAVaultReplicatesIsListedOnceFromItsLiveHost() {
        var devbox = machine("devbox", name: "devbox", sessions: ["01A", "01B"])
        var vault = machine("vault", name: "vault", sessions: ["01A", "01B", "01C"], hosts: [
            FleetHost(hostId: "devbox", hostName: "devbox", online: true, lastSeen: ""),
            FleetHost(hostId: "laptop", hostName: "laptop", online: true, lastSeen: ""),
        ])
        for (index, host) in ["devbox", "devbox", "laptop"].enumerated() { vault.sessions[index].hostId = host }
        func listed() -> [String] {
            Lists(machines: [devbox, vault], sessions: [:]).projects.flatMap(\.sessions).map { "\($0.key.hostId)/\($0.key.sessionId)" }.sorted()
        }

        // The live copies, and the vault's copy of what only the vault reaches.
        #expect(Lists.shadowed([devbox, vault]) == [SessionKey(hostId: "vault", sessionId: "01A"),
                                                     SessionKey(hostId: "vault", sessionId: "01B")])
        #expect(listed() == ["devbox/01A", "devbox/01B", "vault/01C"])

        // devbox drops: the vault's read-only copies stand in for its sessions.
        devbox.connection = .disconnected(error: "gone")
        #expect(listed() == ["vault/01A", "vault/01B", "vault/01C"])
    }
}
