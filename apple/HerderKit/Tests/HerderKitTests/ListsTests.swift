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
            SessionHead(sessionId: $0, hostId: nil, headSeq: 0, status: .idle, parent: nil, task: nil,
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

    @Test func homeKeepsSessionsInTheOrderTheyWereCreatedWhateverTheirState() {
        var older = Script("01A")
        var middle = Script("01B")
        var newer = Script("01C")
        var archived = Script("01D")
        let running = EventBody.sessionStatusChanged(status: .running, retryAt: nil)
        // The older session works on after the others: its activity is the latest.
        let olderModel = older.model([created(), running, .turnStarted(turnId: "t"), .turnStarted(turnId: "u")])
        let newerModel = newer.model([created()])
        #expect(olderModel.updatedAt! > newerModel.updatedAt!)
        let sessions = [
            older.key: olderModel,
            middle.key: middle.model([created(), .sessionStatusChanged(status: .error, retryAt: nil)]),
            newer.key: newerModel,
            archived.key: archived.model([created(), .sessionStatusChanged(status: .archived, retryAt: nil)]),
        ]
        let lists = Lists(machines: [machine("host-a", name: "a", sessions: ["01A", "01B", "01C", "01D"])],
                          sessions: sessions)
        #expect(lists.home.map(\.key.sessionId) == ["01C", "01B", "01A"])
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
    }

    @Test func archivedSessionsFollowTheLiveOnesEachGroupATaskTree() {
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
        // A child whose parent is in the other group leads in its own.
        #expect(project.live.map(\.title) == ["Live child of an archived lead", "Live lead", "Live child"])
        #expect(project.live.map(\.depth) == [0, 0, 1])
        #expect(project.archived.map(\.title) == [
            "Archived lead of a live child", "Archived lead", "Its archived child", "Archived child of a live lead",
        ])
        #expect(project.archived.map(\.depth) == [0, 0, 1, 0])
        #expect(project.archived.allSatisfy { $0.state == .archived })
        #expect(project.sessions.map(\.key) == (project.live + project.archived).map(\.key))
        // A lead still counts all its children, in either group.
        #expect(project.live[1].children == 2)
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
        #expect(lists.home.map(\.key) == [live.key])

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
        #expect(fleet.lists.recent.map(\.key.sessionId) == ["01A"])
    }
}
