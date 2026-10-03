import Foundation
import Herder
@testable import HerderKit
import Testing

func machine(
    _ hostId: HostId, name: String, sessions: [SessionId], projects: [Project] = [], hosts: [FleetHost] = []
) -> Machine {
    Machine(
        hostId: hostId, name: name, addresses: [], fingerprint: "", connection: .connected, role: .owner,
        sessions: sessions.map {
            SessionHead(sessionId: $0, hostId: nil, headSeq: 0, status: .idle, parent: nil, task: nil,
                        title: nil, projectId: nil, accountId: "main", childrenNeedYou: 0)
        },
        hosts: hosts, projects: projects, accounts: [], failover: FailoverSettings(pin: false), terminals: [],
        resources: nil, sessionUsage: [:])
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
        #expect(lists.active.isEmpty)
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
