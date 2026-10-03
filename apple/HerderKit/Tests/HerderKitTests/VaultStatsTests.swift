import Foundation
import Herder
@testable import HerderKit
import Testing

private func pr(_ number: UInt64, _ state: PrState) -> PullRequest {
    PullRequest(number: number, url: "https://github.com/acme/demo/pull/\(number)", title: "PR \(number)",
                headBranch: "b\(number)", state: state, ci: .passing, review: .none, mergeable: .clean)
}

/// A vault with two hosts: `builder` online, `laptop` last seen two hours before `now`.
private let now = Date(timeIntervalSince1970: 1_767_240_000)

private func vault(_ sessions: [(SessionId, host: HostId, status: SessionStatus, project: String?)]) -> Machine {
    var vault = machine("vault", name: "home vault", sessions: sessions.map(\.0), projects: [
        Project(projectId: "github.com/acme/demo", name: "Demo", paths: [], defaultPermissionMode: nil,
                defaultAccount: nil, setupCommand: nil),
    ], hosts: [
        FleetHost(hostId: "h2", hostName: "laptop", online: false,
                  lastSeen: now.addingTimeInterval(-7200).ISO8601Format()),
        FleetHost(hostId: "h1", hostName: "builder", online: true, lastSeen: now.ISO8601Format()),
    ])
    for (index, session) in sessions.enumerated() {
        vault.sessions[index].hostId = session.host
        vault.sessions[index].status = session.status
        vault.sessions[index].projectId = session.project
    }
    return vault
}

struct VaultStatsTests {
    @Test func aMachineWithoutHostsIsNoVault() {
        #expect(VaultStats(machine: machine("host-a", name: "a", sessions: ["01A"]), sessions: [:]) == nil)
    }

    @Test func totalsCountHostsSessionsByStatePRsAndProjects() throws {
        var running = Script("01A", host: "vault")
        var asking = Script("01B", host: "vault")
        let machine = vault([
            ("01A", "h1", .idle, "github.com/acme/demo"),
            ("01B", "h1", .running, "github.com/acme/demo"),
            ("01C", "h2", .idle, "github.com/acme/other"),
            ("01D", "h2", .archived, nil),
        ])
        let sessions = [
            // Its events say running, and outrank the listing's idle.
            running.key: running.model([
                created(), .sessionStatusChanged(status: .running),
                .prLinked(pr: pr(1, .open)), .prLinked(pr: pr(2, .merged)), .prLinked(pr: pr(3, .draft)),
            ]),
            asking.key: asking.model([
                created(), .approvalRequested(approvalId: "a", turnId: "t", toolCallId: "i", summary: "s",
                                              routedTo: .user, reason: nil),
            ]),
        ]
        let stats = try #require(VaultStats(machine: machine, sessions: sessions, now: now))
        #expect(stats.name == "home vault")
        #expect(stats.connection == .connected)
        #expect(stats.hostsOnline == 1)
        #expect(stats.hosts.count == 2)
        #expect(stats.sessions == 4)
        #expect(stats.count(.running) == 1)
        #expect(stats.count(.needsYou) == 1)
        #expect(stats.count(.idle) == 1)
        #expect(stats.count(.archived) == 1)
        #expect(stats.openPRs == 2)
        #expect(stats.projects == 2)
    }

    @Test func eachHostHasItsOwnCountsLastSeenAndTopProjects() throws {
        let machine = vault([
            ("01A", "h1", .running, "github.com/acme/demo"),
            ("01B", "h1", .needsYou, "github.com/acme/demo"),
            ("01C", "h1", .idle, "github.com/acme/other"),
            ("01D", "h2", .idle, nil),
        ])
        let stats = try #require(VaultStats(machine: machine, sessions: [:], now: now))
        // Online hosts first.
        #expect(stats.hosts.map(\.name) == ["builder", "laptop"])
        let builder = stats.hosts[0], laptop = stats.hosts[1]
        #expect(builder.lastSeen == "now")
        #expect(laptop.lastSeen == "2h")
        #expect(builder.sessions == 3)
        #expect(builder.needsYou == 1)
        #expect(builder.byState == [.running: 1, .needsYou: 1, .idle: 1])
        #expect(builder.topProjects == [
            VaultStats.ProjectCount(name: "Demo", sessions: 2), VaultStats.ProjectCount(name: "other", sessions: 1),
        ])
        #expect(laptop.sessions == 1)
        #expect(laptop.topProjects.isEmpty)
    }
}
