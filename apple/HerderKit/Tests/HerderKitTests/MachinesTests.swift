import Foundation
import Herder
@testable import HerderKit
import Testing

/// Three machines and the vault two of them back up to, as the Machines screen shows them:
/// `trash-can-01` has two CLIs behind and a signed-out account, `work-mbp` runs on this Mac and
/// has not reached the vault for six hours.
func sampleFleetMachines(now: Date = .now) -> [Machine] {
    func resources(_ cpu: Double) -> HostResources {
        HostResources(cpuCores: 8, cpuPercent: cpu, load1m: 1, memoryTotalBytes: 16 << 30,
                      memoryAvailableBytes: 8 << 30, pressure: nil, runningTurns: 0, maxTurns: 4, waitingTurns: 0,
                      constraint: nil)
    }
    func account(_ id: String, _ provider: String, signedOut: Bool = false) -> Account {
        Account(accountId: id, provider: provider, label: id, configDir: nil, email: nil, usage: [],
                needsLogin: signedOut ? now.addingTimeInterval(-600).ISO8601Format() : nil)
    }
    func cli(_ provider: String, _ version: String, latest: String) -> ProviderStatus {
        ProviderStatus(provider: provider, installed: true, version: version, latestVersion: latest, binary: provider,
                       canInstall: true, canUpdate: true)
    }
    func running(_ machine: inout Machine, _ count: Int) {
        for index in machine.sessions.indices where index < count { machine.sessions[index].status = .running }
    }

    var mbp = machine("h-mbp", name: "work-mbp", sessions: ["01A", "01B", "01C", "01D"],
                      accounts: ["work", "personal", "team", "ops"].map { account($0, "claude") },
                      providers: [cli("claude", "2.1.310", latest: "2.1.310")])
    mbp.address = "127.0.0.1:7447"
    mbp.quality.lastRttMs = 12
    mbp.resources = resources(40)
    running(&mbp, 3)

    var can = machine("h-can", name: "trash-can-01", sessions: ["01E", "01F"],
                      accounts: [account("work", "claude"), account("ops", "claude", signedOut: true), account("pro", "codex")],
                      providers: [cli("claude", "2.1.303", latest: "2.1.310"), cli("codex", "0.48.0", latest: "0.51.2")])
    can.address = "trash-can-01.tail.ts.net:7447"
    can.quality.lastRttMs = 38
    can.resources = resources(78)
    running(&can, 2)

    var draugelis = machine("h-dr", name: "draugelis", sessions: [], accounts: [account("main", "claude")],
                            providers: [cli("claude", "2.1.310", latest: "2.1.310")], role: .member)
    draugelis.address = "draugelis.lan:7447"
    draugelis.quality.lastRttMs = 61
    draugelis.resources = resources(8)

    let projects = [
        Project(projectId: "github.com/acme/herder", name: "herder", paths: [], defaultPermissionMode: nil,
                defaultAccount: nil, setupCommand: nil),
        Project(projectId: "github.com/acme/trip1", name: "trip1", paths: [], defaultPermissionMode: nil,
                defaultAccount: nil, setupCommand: nil),
    ]
    let stored: [(SessionId, HostId, SessionStatus, String)] = [
        ("02A", "h-mbp", .idle, "github.com/acme/herder"), ("02B", "h-mbp", .archived, "github.com/acme/herder"),
        ("02C", "h-mbp", .archived, "github.com/acme/trip1"), ("02D", "h-can", .idle, "github.com/acme/trip1"),
        ("02E", "h-can", .archived, "github.com/acme/trip1"), ("02F", "h-can", .archived, "github.com/acme/herder"),
        ("02G", "h-can", .running, "github.com/acme/trip1"),
    ]
    var vault = machine("h-vault", name: "herder-vault-0", sessions: stored.map(\.0), projects: projects, hosts: [
        FleetHost(hostId: "h-mbp", hostName: "work-mbp", online: false,
                  lastSeen: now.addingTimeInterval(-6 * 3600).ISO8601Format()),
        FleetHost(hostId: "h-can", hostName: "trash-can-01", online: true, lastSeen: now.ISO8601Format()),
    ])
    for (index, session) in stored.enumerated() {
        vault.sessions[index].hostId = session.1
        vault.sessions[index].status = session.2
        vault.sessions[index].projectId = session.3
    }
    vault.vault = VaultStatus(sessions: UInt64(stored.count), events: 52_000, storageBytes: 245_000_000, hosts: [
        HostReplication(hostId: "h-mbp", sessions: 3, events: 20_000,
                        lastEventAt: now.addingTimeInterval(-6 * 3600 - 120).ISO8601Format(), lagMs: 40, read: true),
        HostReplication(hostId: "h-can", sessions: 4, events: 32_000,
                        lastEventAt: now.addingTimeInterval(-180).ISO8601Format(), lagMs: 25, read: true),
    ])
    vault.quality.lastRttMs = 20
    return [mbp, can, draugelis, vault]
}

@MainActor struct MachineRowTests {
    @Test func aRowPerMachineWithWhatToActOn() throws {
        let machines = sampleFleetMachines()
        let lists = Lists(machines: machines, sessions: [:])
        let rows = MachineRow.rows(machines, summaries: lists.machines, localNames: ["127.0.0.1"])
        // The vault shows in its own section.
        #expect(rows.map(\.name) == ["work-mbp", "trash-can-01", "draugelis"])
        let mbp = rows[0], can = rows[1], draugelis = rows[2]
        #expect(mbp.local && !can.local)
        #expect(mbp.role == "owner" && draugelis.role == "member")
        #expect(mbp.latencyMs == 12)
        #expect(MachinesView.status(can) == "38 ms")
        #expect(can.cpu == 78)
        #expect(mbp.updates == 0)
        #expect(can.updates == 2)
        #expect(can.accounts == 3 && can.signedOut == 1)
        #expect(mbp.backsUpTo == "herder-vault-0" && can.backsUpTo == "herder-vault-0")
        #expect(draugelis.backsUpTo == nil)
    }

    @Test func anOfflineMachineShowsNoStaleLatency() throws {
        var machines = sampleFleetMachines()
        machines[1].connection = .disconnected(error: "refused")
        let row = try #require(MachineRow.rows(machines, summaries: []).first { $0.name == "trash-can-01" })
        #expect(row.latencyMs == nil)
        #expect(MachinesView.status(row) == "Offline")
        #expect(row.statusRank > MachineRow.rows(machines, summaries: [])[0].statusRank)
    }

    @Test func aVaultRowNeedsYouOnlyOnceItsBackupIsAnHourBehind() throws {
        let now = Date.now
        var machines = sampleFleetMachines(now: now)
        let behind = try #require(VaultStats(machine: machines[3], sessions: [:], now: now))
        let line = VaultLine.paired(behind)
        #expect(line.name == "herder-vault-0")
        #expect(line.hosts == 2 && line.sessions == 7)
        #expect(line.size == behind.size)
        // work-mbp went offline six hours ago.
        #expect(line.freshness == "work-mbp 6 h behind")
        #expect(line.isOverdue)
        #expect(line.why?.contains("work-mbp has not reached the vault for 6 h") == true)

        // Back 27 minutes ago: behind, but not yet something to act on.
        machines[3].hosts[0].lastSeen = now.addingTimeInterval(-27 * 60).ISO8601Format()
        let recent = VaultLine.paired(try #require(VaultStats(machine: machines[3], sessions: [:], now: now)))
        #expect(recent.freshness == "work-mbp 27 min behind")
        #expect(!recent.isOverdue)

        machines[3].hosts[0].online = true
        let caughtUp = VaultLine.paired(try #require(VaultStats(machine: machines[3], sessions: [:], now: now)))
        #expect(caughtUp.freshness == "✓ caught up" && caughtUp.behind == 0 && caughtUp.why == nil)
    }

    @Test func anUnpairedVaultRowKnowsOnlyWhoBacksUpToIt() {
        let line = VaultLine.unpaired(UnpairedVault(address: "vault.lan:7447", fingerprint: "aa",
                                                    hosts: ["devbox", "laptop"]))
        #expect(line.name == "vault.lan" && line.hosts == 2)
        #expect(line.sessions == nil && line.size == nil && line.freshness == nil && line.behind == nil)
        #expect(!line.isOverdue)
    }

    @Test func onlyOneMachineIsThisMacThoughSeveralListenOnLoopback() {
        var machines = sampleFleetMachines()
        machines[1].address = "127.0.0.1:7448"
        machines[2].address = "localhost:7449"
        let names: Set<String> = ["localhost", "127.0.0.1", "trash-can-01"]
        // The one named after this Mac.
        #expect(MachineRow.rows(machines, summaries: [], localNames: names).filter(\.local).map(\.name) == ["trash-can-01"])
        // Else the first paired.
        let loopback: Set<String> = ["localhost", "127.0.0.1"]
        #expect(MachineRow.rows(machines, summaries: [], localNames: loopback).filter(\.local).map(\.name) == ["work-mbp"])
        #expect(MachineRow.thisMac(machines, names: ["studio"]) == nil)
    }

    @Test func thePageCountsItsMachinesApartFromTheVault() throws {
        let now = Date.now
        var machines = sampleFleetMachines(now: now)
        let summaries = Lists(machines: machines, sessions: [:]).machines
        let behind = try #require(VaultStats(machine: machines[3], sessions: [:], now: now))
        #expect(MachinesView.status(summaries, vaults: [behind]) == "All 3 machines connected · vault 6 h behind")
        machines[3].hosts[0].online = true
        let caughtUp = try #require(VaultStats(machine: machines[3], sessions: [:], now: now))
        #expect(MachinesView.status(summaries, vaults: [caughtUp]) == "All 3 machines connected · vault caught up")
        #expect(MachinesView.status([], vaults: []) == "No machines yet")
    }

    @Test func thisMacIsTheOneItsAddressNames() {
        let names: Set<String> = ["localhost", "127.0.0.1", "::1", "studio"]
        #expect(MachineRow.isLocal("127.0.0.1:7447", names: names))
        #expect(MachineRow.isLocal("[::1]:7447", names: names))
        #expect(MachineRow.isLocal("Studio:7447", names: names))
        #expect(!MachineRow.isLocal("devbox.lan:7447", names: names))
        #expect(!MachineRow.isLocal(nil, names: names))
    }
}

#if os(macOS)
import AppKit
import SwiftUI

/// Draws the Machines screen with its vault section, and the vault's page, and attaches them;
/// with `HERDER_SCREENSHOT_DIR` set, also writes them there as `machines.png` and
/// `vault-page.png`.
@MainActor struct MachinesScreenshotTests {
    private func fleet() throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        fleet.setMachinesForTesting(sampleFleetMachines())
        return fleet
    }

    @Test func drawsTheMachinesTableAndItsVault() async throws {
        let fleet = try fleet()
        try await shoot(MachinesView(fleet: fleet, sheet: .constant(nil)), named: "machines.png", height: 420)
    }

    @Test func drawsTheVaultPage() async throws {
        let fleet = try fleet()
        try await shoot(VaultPage(fleet: fleet, vault: "h-vault"), named: "vault-page.png", height: 460)
    }

    private func shoot(_ view: some View, named name: String, height: CGFloat) async throws {
        let host = NSHostingView(rootView: view.environment(\.colorScheme, .dark).background(Theme.canvas))
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(x: 0, y: 0, width: 1100, height: height)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        for _ in 0..<5 {
            host.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        #expect(rep.pixelsWide >= 1100)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }
}
#endif
