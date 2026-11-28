#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws Remote access as the app's dark appearance shows it: the Hub tab's row with all well;
/// the page, its summary closed and opened, on each tab, with the hub public behind a
/// front-door login, Tailscale on two machines, the router's VPN seen yesterday and herder's
/// own options (a WireGuard config imported, the hub's WireGuard on, its Cloudflare tunnel up
/// and relay failed) at the Mac's and an iPhone's width; and with nothing set up, and each
/// option offered to its owner. Attaches each; with `HERDER_SCREENSHOT_DIR` set, also writes
/// them there as `remote-access-<screen>.png`.
@MainActor @Suite(.serialized) struct RemoteAccessScreenshotTests {
    private let now = Date(timeIntervalSince1970: 1_800_000_000)

    /// Away from home: connected to the hub through its tunnel, trash-can-01 over Tailscale,
    /// work-mbp with a Tailscale address too; the router's VPN worked yesterday.
    private var working: FleetReach {
        var machines = remoteFleet(now: now)
        machines[3].address = "wss://hub.example.com"
        machines[0].addresses = ["172.20.10.5:7447", "100.101.1.2:7447"]
        machines[0].address = "hub://h-hub"
        machines[3].frontDoors = [FrontDoor(address: "wss://hub.example.com", expiresAt: "2096-10-02T07:06:40Z",
                                            signIn: false)]
        var options = RemoteOptions(hub: "h-hub", owner: true, tunnels: [homeTunnel(up: true)])
        options.read(remoteOffer())
        return FleetReach(machines: machines, seen: ["vpn:172.20.40.0/24": now.addingTimeInterval(-86400),
                                                       "vpn:172.20.10.0/24": now.addingTimeInterval(-86400)],
                            options: options, now: now)
    }

    /// Each machine's route in use and round trip, work-mbp pinned to the hub's tunnel.
    private var routes: [MachineRoutes] {
        var machines = remoteFleet(now: now)
        machines[3].address = "wss://hub.example.com"
        machines[0].addresses = ["172.20.10.5:7447", "100.101.1.2:7447", "hub://h-hub"]
        machines[0].address = "hub://h-hub"
        machines[0].pinnedAddress = "hub://h-hub"
        return machines.map { MachineRoutes($0, among: machines) }
    }

    /// Machines on the home network only, and no hub.
    private var nothing: FleetReach {
        let machines = Array(sampleFleetMachines(now: now).prefix(3)).map { machine in
            var machine = machine
            machine.addresses = ["172.20.40.\(machine.name.count):7447"]
            machine.address = machine.addresses.first
            return machine
        }
        return FleetReach(machines: machines, now: now)
    }

    /// At home with a hub this device owns and nothing of herder's set up yet: each option's
    /// set-up offered.
    private var offers: FleetReach {
        var machines = remoteFleet(now: now)
        machines[3].addresses = ["172.20.40.200:7448"]
        var options = RemoteOptions(hub: "h-hub", owner: true)
        options.answered = true
        return FleetReach(machines: machines, options: options, now: now)
    }

    @Test func drawsTheHubTabsRow() async throws {
        let row = RemoteAccessSection(access: working) {}.padding(16)
        try await shoot(row, size: CGSize(width: 390, height: 130), named: "remote-access-hub-iphone.png")
    }

    /// Each tab with all set up, at the Mac's and an iPhone's width.
    @Test(arguments: RemoteTab.allCases) func drawsEachTab(_ tab: RemoteTab) async throws {
        let name = switch tab {
        case .ownVpn: "your-vpn"
        case .hubWireGuard: "hub-wireguard"
        case .cloudflare: "cloudflare"
        case .others: "others"
        }
        try await shoot(RemoteAccessView(access: working, machines: routes, tab: tab) { _ in },
                        size: CGSize(width: 1040, height: 1400), named: "remote-access-\(name)-mac.png")
        try await shoot(RemoteAccessView(access: working, machines: routes, tab: tab) { _ in },
                        size: CGSize(width: 390, height: 1700), named: "remote-access-\(name)-iphone.png")
    }

    /// The summary opened: the hub's public address and each machine's route, to rank or pin.
    @Test func drawsTheSummaryOpened() async throws {
        try await shoot(RemoteAccessView(access: working, machines: routes, showsDetail: true) { _ in },
                        size: CGSize(width: 1040, height: 1700), named: "remote-access-summary-mac.png")
        try await shoot(RemoteAccessView(access: working, machines: routes, showsDetail: true) { _ in },
                        size: CGSize(width: 390, height: 2000), named: "remote-access-summary-iphone.png")
    }

    @Test func drawsNothingSetUpOnAnIPhone() async throws {
        try await shoot(RemoteAccessView(access: nothing) { _ in }, size: CGSize(width: 390, height: 1400),
                        named: "remote-access-none-iphone.png")
    }

    /// A hub this device owns with nothing of herder's set up: the hub's WireGuard and
    /// Cloudflare offered.
    @Test func drawsTheOptionsOfferedToAnOwnerOnAnIPhone() async throws {
        try await shoot(RemoteAccessView(access: offers, tab: .hubWireGuard) { _ in }, size: CGSize(width: 390, height: 1400),
                        named: "remote-access-offers-hub-wireguard-iphone.png")
        try await shoot(RemoteAccessView(access: offers, tab: .cloudflare) { _ in }, size: CGSize(width: 390, height: 1700),
                        named: "remote-access-offers-cloudflare-iphone.png")
    }

    private func shoot(_ content: some View, size: CGSize, named name: String) async throws {
        let view = content
            .frame(width: size.width, height: size.height, alignment: .topLeading)
            .background(Theme.canvas)
            .environment(\.colorScheme, .dark)
        let host = NSHostingView(rootView: view)
        host.appearance = NSAppearance(named: .darkAqua)
        host.frame = NSRect(origin: .zero, size: size)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
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
}
#endif
