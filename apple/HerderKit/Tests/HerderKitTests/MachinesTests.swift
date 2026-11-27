import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Three machines and the hub two of them back up to, as the Machines screen shows them:
/// `trash-can-01` has two CLIs behind and a signed-out account, `work-mbp` runs on this Mac and
/// has not reached the hub for six hours.
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
    var hub = machine("h-hub", name: "herder-hub-0", sessions: stored.map(\.0), projects: projects, hosts: [
        FleetHost(hostId: "h-mbp", hostName: "work-mbp", online: false,
                  lastSeen: now.addingTimeInterval(-6 * 3600).ISO8601Format()),
        FleetHost(hostId: "h-can", hostName: "trash-can-01", online: true, lastSeen: now.ISO8601Format()),
    ])
    for (index, session) in stored.enumerated() {
        hub.sessions[index].hostId = session.1
        hub.sessions[index].status = session.2
        hub.sessions[index].projectId = session.3
    }
    hub.hub = HubStatus(sessions: UInt64(stored.count), events: 52_000, storageBytes: 245_000_000, hosts: [
        HostReplication(hostId: "h-mbp", sessions: 3, events: 20_000,
                        lastEventAt: now.addingTimeInterval(-6 * 3600 - 120).ISO8601Format(), lagMs: 40),
        HostReplication(hostId: "h-can", sessions: 4, events: 32_000,
                        lastEventAt: now.addingTimeInterval(-180).ISO8601Format(), lagMs: 25),
    ])
    hub.quality.lastRttMs = 20
    return [mbp, can, draugelis, hub]
}

@MainActor struct MachineRowTests {
    @Test func aRowPerMachineWithHowItIsReached() throws {
        let machines = sampleFleetMachines()
        let lists = Lists(machines: machines, sessions: [:])
        let rows = MachineRow.rows(machines, summaries: lists.machines, localNames: ["127.0.0.1"])
        // By name; the hub is not a machine.
        #expect(rows.map(\.name) == ["draugelis", "trash-can-01", "work-mbp"])
        let draugelis = rows[0], can = rows[1], mbp = rows[2]
        #expect(mbp.local && !can.local)
        // Only being a member is said, as it is what limits you.
        #expect(draugelis.member && !mbp.member && !can.member)
        // Each connected row names the route it is reached by.
        #expect(rows.map(\.reach.label) == ["Local network", "Tailscale", "Local network"])
    }

    @Test func machinesHangUnderTheHubTheySyncTo() {
        var machines = sampleFleetMachines()
        let structure = SetupStructure(machines: machines, summaries: [], localNames: [])
        #expect(structure.hubs.map(\.name) == ["herder-hub-0"])
        let hub = structure.hubs[0]
        #expect(hub.id == "paired.h-hub" && hub.hostId == "h-hub" && hub.connection == .connected)
        #expect(hub.machines.map(\.name) == ["trash-can-01", "work-mbp"])
        // draugelis syncs to no hub.
        #expect(structure.others.map(\.name) == ["draugelis"])
        #expect(structure.machines == 3)
        #expect(hub.keeps == "Keeps copies of 7 sessions, your secrets and provider logins")
        #expect(hub.invitation == nil)

    }

    /// However the hub is exposed (a port forward, a reverse proxy, a tunnel, a VPN), Hub says
    /// how far its furthest-reaching address reaches, and names that address.
    @Test func aHubSaysHowFarItIsReachedAndAtWhichAddress() {
        var machines = sampleFleetMachines()
        func reachability(_ addresses: [String]) -> String {
            machines[3].addresses = addresses
            return SetupStructure(machines: machines, summaries: [], localNames: []).hubs[0].reachability
        }
        #expect(reachability([]) == "Reachable on its network only")
        #expect(reachability(["10.0.0.4:7447", "hub.lan:7447", "[fe80::1]:7447"]) == "Reachable on its network only")
        #expect(reachability(["10.0.0.4:7447", "hub.tail1234.ts.net:7447"])
            == "Reachable over your VPN, at hub.tail1234.ts.net")
        #expect(reachability(["100.101.4.2:7447"]) == "Reachable over your VPN, at 100.101.4.2")
        // A forwarded port, a reverse proxy and a tunnel all read the same.
        #expect(reachability(["10.0.0.4:7447", "203.0.113.7:7447"]) == "Reachable from anywhere, at 203.0.113.7")
        #expect(reachability(["hub.tail1234.ts.net:7447", "hub.example.com:443"])
            == "Reachable from anywhere, at hub.example.com")
        #expect(reachability(["wss://Hub.Example.com:8443/herder", "10.0.0.4:7447"])
            == "Reachable from anywhere, at hub.example.com")
        #expect(AddressScope("[fd7a:115c:a1e0::12]:7447") == .vpn && AddressScope("172.20.1.1:7447") == .network)
    }

    /// A laptop asleep is expected: it reads Offline, and On another network while its hub
    /// still hears from it.
    @Test func aMachineThisDeviceCannotReachIsElsewhereWhileItsHubHearsFromIt() {
        var machines = sampleFleetMachines()
        // The hub hears from trash-can-01, and has not heard from work-mbp for six hours.
        machines[0].connection = .disconnected(error: "refused")
        machines[1].connection = .disconnected(error: "refused")
        let rows = SetupStructure(machines: machines, summaries: [], localNames: []).hubs[0].machines
        #expect(rows.map(\.name) == ["trash-can-01", "work-mbp"])
        #expect(rows.map(\.reach) == [.elsewhere, .offline])
        #expect(rows.map(\.reach.label) == ["On another network", "Offline"])

        // A hub this device has lost says nothing current.
        machines[3].connection = .disconnected(error: "refused")
        let lost = SetupStructure(machines: machines, summaries: [], localNames: []).hubs[0]
        #expect(lost.machines.map(\.reach) == [.offline, .offline])
        #expect(MachineReach(.connecting, heardByHub: true) == .connecting)
    }

    @Test func aHubNotOnThisDeviceNamesItsMachinesAndOffersSignIn() {
        let machines = Array(sampleFleetMachines().prefix(3))
        let unpaired = UnpairedHub(address: "hub.lan:7447", fingerprint: "aa", hosts: ["trash-can-01"])
        let structure = SetupStructure(machines: machines, summaries: [], unpaired: [unpaired], localNames: [])
        let hub = structure.hubs[0]
        #expect(hub.id == "unpaired.aa" && hub.name == "hub.lan")
        // It has no page and no connection until this device signs in to it.
        #expect(hub.hostId == nil && hub.connection == nil && hub.unpaired == unpaired)
        #expect(hub.invitation == "trash-can-01 syncs to it. Sign in to see it here.")
        #expect(hub.machines.map(\.name) == ["trash-can-01"])
        #expect(structure.others.map(\.name) == ["draugelis", "work-mbp"])
    }

    @Test func withNoHubEveryMachineIsListedOnItsOwn() {
        let machines = Array(sampleFleetMachines().prefix(3))
        let structure = SetupStructure(machines: machines, summaries: [], localNames: [])
        #expect(structure.hubs.isEmpty)
        #expect(structure.others.map(\.name) == ["draugelis", "trash-can-01", "work-mbp"])
    }

    /// The rail runs past a row to the next, and ends at the last row's middle.
    @Test func theRailEndsAtTheLastMachine() {
        let rect = CGRect(x: 0, y: 0, width: 24, height: 58)
        #expect(HubRail(last: false, gap: 6).path(in: rect).boundingRect.maxY == 58)
        #expect(HubRail(last: true, gap: 6).path(in: rect).boundingRect.maxY == 32)
        #expect(HubRail(last: true, gap: 6).path(in: rect).boundingRect.maxX == 24)
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

    @Test func thisMacIsTheOneItsAddressNames() {
        let names: Set<String> = ["localhost", "127.0.0.1", "::1", "studio"]
        #expect(MachineRow.isLocal("127.0.0.1:7447", names: names))
        #expect(MachineRow.isLocal("[::1]:7447", names: names))
        #expect(MachineRow.isLocal("Studio:7447", names: names))
        #expect(!MachineRow.isLocal("devbox.lan:7447", names: names))
        #expect(!MachineRow.isLocal(nil, names: names))
    }

    @Test func thisMacAnswersToItsHostNameWithAndWithoutLocal() {
        let names: Set<String> = ["localhost", "127.0.0.1", "::1", "studio", "studio.local"]
        #expect(MachineRow.names(host: "Studio") == names)
        #expect(MachineRow.names(host: "studio.local") == names)
    }

    #if os(macOS)
    /// This Mac's names come from the kernel, as its daemon names its machine (`gethostname`),
    /// not from a DNS lookup, which held the main thread on each draw of the sidebar.
    @Test func thisMacGoesByTheNameItsDaemonGivesIt() throws {
        let hostname = Process()
        hostname.executableURL = URL(fileURLWithPath: "/bin/hostname")
        let out = Pipe()
        hostname.standardOutput = out
        try hostname.run()
        hostname.waitUntilExit()
        let name = String(decoding: out.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        #expect(MachineRow.localNames == MachineRow.names(host: name))
    }
    #endif
}

#if os(macOS)
import AppKit

/// Draws Hub's hub with its machines under it, and the hub's page, and attaches them;
/// with `HERDER_SCREENSHOT_DIR` set, also writes them there as `machines.png` and
/// `hub-page.png`.
@MainActor struct MachinesScreenshotTests {
    private func fleet() throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        fleet.setMachinesForTesting(sampleFleetMachines())
        return fleet
    }

    @Test func drawsTheHubAndItsMachines() async throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        // The hub still hears from trash-can-01, which this Mac cannot reach.
        var machines = sampleFleetMachines()
        machines[1].connection = .disconnected(error: "no answer in 10s")
        fleet.setMachinesForTesting(machines)
        let view = MachinesView(fleet: fleet, sheet: .constant(nil)).padding(20)
            .frame(width: 1100, height: 420, alignment: .top)
        try await shoot(view, named: "machines.png", height: 420)
    }

    @Test func drawsTheHubPage() async throws {
        let fleet = try fleet()
        try await shoot(HubPage(fleet: fleet, hub: "h-hub"), named: "hub-page.png", height: 460)
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
