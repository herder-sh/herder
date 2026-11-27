#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws the machine page as the app's dark appearance shows it, for a machine the user owns
/// (a signed-out account, two CLI updates, a login missing there, backing up to a hub) and
/// for one they are a member on, and one they own linked to no hub, and attaches each; with
/// `HERDER_SCREENSHOT_DIR` set, also writes them there as `machine-owner.png`,
/// `machine-member.png` and `machine-unlinked.png`, an owner's public addresses as
/// `machine-public-addresses.png`, and the proxy headers sheet as `address-headers.png`.
@MainActor struct MachinePageScreenshotTests {
    @Test func drawsTheOwnersPage() async throws {
        let fleet = try sampleFleet()
        fleet.backupLinks["trash-can-01"] = .linked(LinkedHub(address: "10.0.0.9:7447", fingerprint: "ab12",
                                                                backup: true, secrets: true))
        try await shoot(MachinePage(fleet: fleet, hostId: "trash-can-01", sheet: .constant(nil)),
                        crumbs: ["Hub", "trash-can-01"], height: 1900, named: "machine-owner.png")
    }

    @Test func drawsAnUnlinkedPage() async throws {
        let fleet = try sampleFleet()
        fleet.backupLinks["work-mbp"] = .nowhere(localHub: false)
        try await shoot(MachinePage(fleet: fleet, hostId: "work-mbp", sheet: .constant(nil)),
                        crumbs: ["Hub", "work-mbp"], height: 1700, named: "machine-unlinked.png")
    }

    @Test func drawsAMembersPage() async throws {
        try await shoot(MachinePage(fleet: try sampleFleet(), hostId: "team-box", sheet: .constant(nil)), crumbs: ["Hub", "team-box"],
                        height: 1000, named: "machine-member.png")
    }

    @Test func drawsThePublicAddresses() async throws {
        let fleet = try sampleFleet()
        let machine = try #require(fleet.machines.first { $0.hostId == "trash-can-01" })
        let section = PublicAddressesSection(fleet: fleet, machine: machine,
                                             addresses: ["home.example.net:7447", "wss://hub.example.com"])
            .padding(24)
            .frame(width: 720)
            .background(Theme.canvas)
            .environment(\.colorScheme, .dark)
        let host = NSHostingView(rootView: section)
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(x: 0, y: 0, width: 720, height: 340)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        host.layoutSubtreeIfNeeded()
        try await Task.sleep(for: .milliseconds(100))
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: "machine-public-addresses.png")
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("machine-public-addresses.png"))
        }
    }

    @Test func drawsTheProxyHeadersSheet() async throws {
        let target = HeaderTarget(hostId: "trash-can-01", address: "wss://hub.example.com",
                                  names: ["cf-access-client-id", "cf-access-client-secret"])
        let sheet = AddressHeadersSheet(fleet: try sampleFleet(), target: target)
            .background(Theme.canvas)
            .environment(\.colorScheme, .dark)
        let host = NSHostingView(rootView: sheet)
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(x: 0, y: 0, width: 560, height: 420)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        host.layoutSubtreeIfNeeded()
        try await Task.sleep(for: .milliseconds(100))
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: "address-headers.png")
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("address-headers.png"))
        }
    }

    private func shoot(_ page: MachinePage, crumbs: [String], height: CGFloat, named name: String) async throws {
        let view = VStack(spacing: 0) {
            PageTrail(crumbs: crumbs) { _ in }
            page
        }
        .background(Theme.canvas)
        .environment(\.colorScheme, .dark)
        let host = NSHostingView(rootView: view)
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(x: 0, y: 0, width: 1040, height: height)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        // The index shows once the page has read its sections.
        for _ in 0..<5 {
            host.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        Attachment.record(png, named: name)
        if let dir = ProcessInfo.processInfo.environment["HERDER_SCREENSHOT_DIR"] {
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
    }

    private func sampleFleet() throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        let reset = Date.now.addingTimeInterval(3 * 3600).formatted(.iso8601)
        func account(_ id: String, _ provider: Provider, _ label: String, used: Double = 0,
                     fallback: Bool = false, needsLogin: String? = nil) -> Account {
            Account(accountId: id, provider: provider, label: label, configDir: nil, email: nil,
                    usage: needsLogin == nil ? [UsageWindow(window: "five_hour", usedPercent: used, resetsAt: reset)] : [],
                    fallback: fallback, needsLogin: needsLogin)
        }
        func cli(_ provider: Provider, _ version: String, latest: String? = nil) -> ProviderStatus {
            ProviderStatus(provider: provider, installed: true, version: version, latestVersion: latest, binary: nil,
                           canInstall: true, canUpdate: true)
        }
        let resources = HostResources(cpuCores: 8, cpuPercent: 20, load1m: 1, memoryTotalBytes: 16 << 30,
                                      memoryAvailableBytes: 9 << 30, pressure: nil, runningTurns: 2, maxTurns: 4,
                                      waitingTurns: 0, constraint: nil)
        let signedOut = Date.now.addingTimeInterval(-14 * 60).formatted(.iso8601)
        var owned = machine("trash-can-01", name: "trash-can-01", sessions: ["01A", "01B", "01C"],
                            accounts: [account("claude-work", "claude", "work", used: 82),
                                       account("claude-ops", "claude", "ops", fallback: true, needsLogin: signedOut),
                                       account("codex", "codex", "Pro", used: 9)],
                            providers: [cli("claude", "2.1.303 (Claude Code)", latest: "2.1.310"),
                                        cli("codex", "codex-cli 0.48.0", latest: "0.51.2")])
        owned.sessions[0].accountId = "claude-ops"
        owned.sessions[0].status = .needsLogin
        owned.sessions[1].accountId = "claude-ops"
        owned.sessions[1].status = .needsLogin
        owned.addresses = ["trash-can-01.tailnet.ts.net:7447", "192.168.1.20:7447"]
        owned.address = owned.addresses[0]
        owned.fingerprint = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        owned.quality.lastRttMs = 38
        owned.resources = resources
        var laptop = machine("work-mbp", name: "work-mbp", sessions: [],
                             accounts: [account("claude-work", "claude", "work", used: 82),
                                        account("cursor", "cursor", "Cursor")],
                             providers: [cli("claude", "2.1.310 (Claude Code)"), cli("codex", "codex-cli 0.51.2")])
        laptop.resources = resources
        var member = machine("team-box", name: "team-box", sessions: ["01D"],
                             accounts: [account("claude-team", "claude", "team", used: 40)],
                             providers: [cli("claude", "2.1.303 (Claude Code)", latest: "2.1.310")], role: .member)
        member.resources = resources
        member.quality.lastRttMs = 61
        member.addresses = ["team-box.tailnet.ts.net:7447"]
        var hub = machine("herder-hub-0", name: "herder-hub-0", sessions: [],
                            hosts: [FleetHost(hostId: "trash-can-01", hostName: "trash-can-01", online: true,
                                              lastSeen: Date.now.formatted(.iso8601))])
        hub.fingerprint = "ab12"
        hub.hub = HubStatus(sessions: 40, events: 9000, storageBytes: 1 << 26, hosts: [
            HostReplication(hostId: "trash-can-01", sessions: 40, events: 9000,
                            lastEventAt: Date.now.addingTimeInterval(-180).formatted(.iso8601), lagMs: 300),
        ])
        fleet.setMachinesForTesting([owned, laptop, member, hub])
        return fleet
    }
}
#endif
