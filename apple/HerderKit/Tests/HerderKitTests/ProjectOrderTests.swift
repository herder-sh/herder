import Foundation
import Herder
@testable import HerderKit
import Testing

/// A project with live sessions in `states`, `recency` its place by last activity.
private func project(
    _ name: String, recency: Int, states: [SessionState] = [], quiet: Bool = false
) -> ProjectGroup {
    let live = states.enumerated().map { index, state in
        SessionSummary(key: SessionKey(hostId: "h", sessionId: "\(name)-\(index)"), title: "", project: name,
                       branch: "", machine: "h", state: state, activity: "", age: "")
    }
    return ProjectGroup(projectId: "acme/\(name)", name: name, live: live, archived: [],
                        recency: recency, quiet: quiet)
}

private func summary(_ name: String, _ connection: ConnectionState, running: Int = 0) -> MachineSummary {
    MachineSummary(hostId: name, name: name, connection: connection, role: .owner, cpu: nil, memory: nil,
                   running: running, sessions: 0, accounts: [], hosts: [], pinned: false)
}

struct ProjectOrderTests {
    let projects = [
        project("charlie", recency: 0, states: [.running, .running]),
        project("alpha", recency: 2),
        project("bravo", recency: 1, states: [.needsYou, .running]),
        project("delta", recency: 3, states: [.needsYou]),
        ProjectGroup(projectId: nil, name: "No project yet", live: [], archived: [], recency: 4),
    ]

    private func names(_ sort: ProjectSort, manual: [String] = []) -> [String] {
        ProjectOrder(sort: sort, manual: manual).sorted(projects).map(\.name)
    }

    @Test func sortsFourWaysWithTheProjectlessSessionsLast() {
        #expect(names(.name) == ["alpha", "bravo", "charlie", "delta", "No project yet"])
        #expect(names(.recent) == ["charlie", "bravo", "alpha", "delta", "No project yet"])
        // What waits for you most, then what works most, then the most recent.
        #expect(names(.needsYou) == ["bravo", "delta", "charlie", "alpha", "No project yet"])
        // By hand: as dragged, the projects never dragged after them by name.
        #expect(names(.manual, manual: ["acme/delta", "acme/charlie"]) == ["delta", "charlie", "alpha", "bravo", "No project yet"])
        #expect(names(.manual) == names(.name))
    }

    @Test func theManualOrderSurvivesBeingStored() {
        let stored = ["delta", "charlie"].map { "acme/\($0)" }.joined(separator: "\n")
        let order = ProjectOrder(sort: .manual, stored: stored)
        #expect(order.manual == ["acme/delta", "acme/charlie"])
        #expect(ProjectOrder(sort: .manual, stored: "").manual.isEmpty)
        #expect(ProjectSort(rawValue: ProjectSort.needsYou.rawValue) == .needsYou)
    }

    @Test func draggingAProjectTakesTheTargetsPlace() {
        let ids = ["a", "b", "c", "d"]
        #expect(ProjectOrder.move("d", onto: "b", in: ids) == ["a", "d", "b", "c"])
        #expect(ProjectOrder.move("a", onto: "c", in: ids) == ["b", "c", "a", "d"])
        #expect(ProjectOrder.move("a", onto: "a", in: ids) == ids)
        #expect(ProjectOrder.move("x", onto: "a", in: ids) == ids)
    }

    @Test func quietProjectsFoldUnlessTheyNeedYouOrAreOpen() {
        let projects = [
            project("active", recency: 0),
            project("quiet", recency: 1, quiet: true),
            project("waiting", recency: 2, states: [.needsYou], quiet: true),
            project("open", recency: 3, quiet: true),
        ]
        let (shown, folded) = ProjectOrder(sort: .recent, manual: []).folded(projects, open: .project("acme/open"))
        #expect(shown.map(\.name) == ["active", "waiting", "open"])
        #expect(folded.map(\.name) == ["quiet"])
    }

    @Test func aProjectIsQuietAfterThirtyDaysWithoutActivity() {
        var script = Script("01A")
        var host = machine("host-a", name: "a", sessions: ["01A"])
        host.sessions[0].projectId = "acme/app"
        let idle = Project(projectId: "acme/idle", name: "idle", paths: [], defaultPermissionMode: nil,
                           defaultAccount: nil, setupCommand: nil)
        host.projects = [idle]
        let sessions = [script.key: script.model([created()])]
        // The script's events are stamped on 2026-01-01.
        let soon = Lists(machines: [host], sessions: sessions, now: Date(timeIntervalSince1970: 1_767_225_600 + 29 * 86400))
        let later = Lists(machines: [host], sessions: sessions, now: Date(timeIntervalSince1970: 1_767_225_600 + 31 * 86400))
        #expect(soon.projects.map(\.quiet) == [false, true])
        #expect(later.projects.map(\.quiet) == [true, true])
    }

    @Test func projectsCountOnlyWhatIsLive() {
        let busy = project("busy", recency: 0, states: [.needsYou, .running, .running, .idle, .error])
        #expect(busy.needsYou == 1)
        #expect(busy.working == 2)
        #expect(project("idle", recency: 0, states: [.idle, .done]).needsYou == 0)
    }

    @Test func theMachinesFooterSaysHowManyAreNotWell() {
        #expect(MachinesFooter.summary([]) == "No machines")
        #expect(MachinesFooter.summary([summary("a", .connected)]) == "1 machine")
        #expect(MachinesFooter.summary([
            summary("a", .connected), summary("b", .disconnected(error: "gone")), summary("c", .connecting),
            summary("d", .connected),
        ]) == "4 machines · 1 offline · 1 connecting")
        #expect(MachinesFooter.status(summary("a", .connected, running: 2)) == "2 running")
        #expect(MachinesFooter.status(summary("a", .connected)) == "Idle")
    }
}
