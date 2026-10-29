import Foundation
import Herder
@testable import HerderKit
import Testing

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
    @Test func emptyVaultStillShowsItsSize() throws {
        var machine = machine("vault", name: "vault", sessions: [])
        machine.vault = VaultStatus(sessions: 0, events: 12, storageBytes: 4096, hosts: [])
        let stats = try #require(VaultStats(machine: machine, sessions: [:]))
        #expect(stats.hosts.isEmpty)
        #expect(stats.size == ByteCountFormatter.string(fromByteCount: 4096, countStyle: .file))
        #expect(stats.furthestBehind == nil)
    }

    @Test func freshnessIsRelativeAndSaysWhyAHostIsBehind() async throws {
        var machine = vault([])
        machine.vault = VaultStatus(sessions: 0, events: 12, storageBytes: 4096, hosts: [
            // builder's last batch arrived 23 ms after its newest event, three minutes ago.
            HostReplication(hostId: "h1", sessions: 0, events: 12,
                            lastEventAt: now.addingTimeInterval(-180).ISO8601Format(), lagMs: 23),
            HostReplication(hostId: "h2", sessions: 0, events: 0, lastEventAt: nil, lagMs: nil),
        ])
        let stats = try #require(VaultStats(machine: machine, sessions: [:], now: now))
        let builder = try #require(stats.hosts.first { $0.id == "h1" })
        #expect(builder.freshness.label(now: now) == "✓ caught up")
        #expect(builder.freshness.why(host: builder.name, now: now) == nil)
        #expect(builder.lastBatch.map { abs($0.timeIntervalSince(now.addingTimeInterval(-180))) < 1 } == true)
        // laptop went offline two hours ago.
        let laptop = try #require(stats.hosts.first { $0.id == "h2" })
        #expect(laptop.freshness.label(now: now) == "2 h behind")
        #expect(laptop.freshness.why(host: laptop.name, now: now)?.contains("laptop has not reached the vault for 2 h") == true)
        #expect(stats.furthestBehind?.id == "h2")
        #expect(stats.lastBatch == builder.lastBatch)
        // Never a raw timestamp.
        let status = await MainActor.run { stats.status }
        #expect(status.hasPrefix("Connected · backing up 2 hosts · last batch "))
        #expect(!status.contains("2026-"))
    }

    @Test func anOnlineHostSendingABacklogIsBehindByItsLag() {
        let freshness = Freshness(online: true, lastSeen: now, lagMs: 6 * 3_600_000, lastEvent: nil, now: now)
        #expect(freshness.label(now: now) == "6 h behind")
        #expect(freshness.why(host: "work-mbp", now: now)?.contains("still sending") == true)
        // A batch a minute late is still caught up.
        #expect(Freshness(online: true, lastSeen: now, lagMs: 60_000, lastEvent: nil, now: now).behind(now: now) == 0)
        #expect(Freshness.span(30) == "1 min")
        #expect(Freshness.span(180) == "3 min")
        #expect(Freshness.span(3 * 86400) == "3 d")
    }

    @Test func onlyAnOwnersVaultIsOwned() throws {
        var machine = vault([])
        #expect(try #require(VaultStats(machine: machine, sessions: [:])).owned)
        machine.role = .member
        #expect(try #require(VaultStats(machine: machine, sessions: [:])).owned == false)
    }

    @Test func aMachineWithoutHostsIsNoVault() {
        #expect(VaultStats(machine: machine("host-a", name: "a", sessions: ["01A"]), sessions: [:]) == nil)
    }

    @Test func totalsCountSessionsByState() throws {
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
            running.key: running.model([created(), .sessionStatusChanged(status: .running, retryAt: nil)]),
            asking.key: asking.model([
                created(), .approvalRequested(approvalId: "a", turnId: "t", toolCallId: "i", summary: "s",
                                              routedTo: .user, reason: nil, secret: nil),
            ]),
        ]
        let stats = try #require(VaultStats(machine: machine, sessions: sessions, now: now))
        #expect(stats.name == "home vault")
        #expect(stats.connection == .connected)
        #expect(stats.hosts.count == 2)
        #expect(stats.sessions == 4)
        #expect(stats.count(.running) == 1)
        #expect(stats.count(.needsYou) == 1)
        #expect(stats.count(.idle) == 1)
        #expect(stats.count(.archived) == 1)
        #expect(VaultOverview.breakdown(stats.byState) == "1 running · 1 needs you · 1 idle · 1 archived")
    }

    @Test func eachHostHasItsOwnCountAndTopProjects() throws {
        let machine = vault([
            ("01A", "h1", .running, "github.com/acme/demo"),
            ("01B", "h1", .needsYou, "github.com/acme/demo"),
            ("01C", "h1", .idle, "github.com/acme/other"),
            ("01D", "h2", .idle, nil),
        ])
        let stats = try #require(VaultStats(machine: machine, sessions: [:], now: now))
        #expect(stats.hosts.map(\.name) == ["builder", "laptop"])
        let builder = stats.hosts[0], laptop = stats.hosts[1]
        #expect(builder.sessions == 3)
        #expect(builder.topProjects == [
            VaultStats.ProjectCount(name: "Demo", sessions: 2), VaultStats.ProjectCount(name: "other", sessions: 1),
        ])
        #expect(laptop.sessions == 1)
        #expect(laptop.topProjects.isEmpty)
    }
}

struct UnpairedVaultTests {
    @Test func groupsHostsByVaultAndLeavesOutPairedOnes() {
        let home = LinkedVault(address: "vault.lan:7447", fingerprint: "aa", backup: true, secrets: true)
        let work = LinkedVault(address: "backup.example.com:7447", fingerprint: "bb", backup: true, secrets: true)
        let vaults = UnpairedVault.from(
            [(host: "laptop", vault: home), (host: "Builder", vault: home), (host: "devbox", vault: work)],
            paired: [])
        #expect(vaults.map(\.name) == ["backup.example.com", "vault.lan"])
        #expect(vaults.map(\.hosts) == [["devbox"], ["Builder", "laptop"]])
        #expect(vaults[0].backsUp == "devbox backs up here")
        #expect(vaults[1].backsUp == "Builder and laptop back up here")

        // Once this device pairs with a vault, its statistics show instead.
        let rest = UnpairedVault.from([(host: "laptop", vault: home), (host: "devbox", vault: work)], paired: ["aa"])
        #expect(rest.map(\.fingerprint) == ["bb"])
    }

    @Test func namesAVaultAfterTheMachineThatReachesItOnLoopback() {
        let local = LinkedVault(address: "127.0.0.1:7448", fingerprint: "aa", backup: true, secrets: true)
        let remote = LinkedVault(address: "draugelis.lan:7448", fingerprint: "aa", backup: true, secrets: true)
        let vaults = UnpairedVault.from([(host: "laptop", vault: remote), (host: "draugelis", vault: local)], paired: [])
        #expect(vaults.map(\.name) == ["draugelis’s vault"])
        #expect(vaults.map(\.machine) == ["draugelis"])
    }
}
