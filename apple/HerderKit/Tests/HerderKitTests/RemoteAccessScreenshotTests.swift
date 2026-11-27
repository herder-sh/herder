#if os(macOS)
import AppKit
import Foundation
import Herder
@testable import HerderKit
import SwiftUI
import Testing

/// Draws Remote access as the app's dark appearance shows it: the Hub tab's row with all well;
/// the page with the hub public behind a front-door login, Tailscale on two machines and the
/// router's VPN seen yesterday, at the Mac's and an iPhone's width; and with nothing set up,
/// every route's steps open. Attaches each; with `HERDER_SCREENSHOT_DIR` set, also writes them
/// there as `remote-access-<screen>.png`.
@MainActor @Suite(.serialized) struct RemoteAccessScreenshotTests {
    private let now = Date(timeIntervalSince1970: 1_800_000_000)

    /// Away from home: connected to the hub through its tunnel, trash-can-01 over Tailscale,
    /// work-mbp with a Tailscale address too; the router's VPN worked yesterday.
    private var working: RemoteAccess {
        var machines = remoteFleet(now: now)
        machines[3].address = "wss://hub.example.com"
        machines[0].addresses = ["172.20.10.5:7447", "100.101.1.2:7447"]
        machines[0].address = "hub://h-hub"
        return RemoteAccess(machines: machines, seen: ["vpn:172.20.40.0/24": now.addingTimeInterval(-86400),
                                                       "vpn:172.20.10.0/24": now.addingTimeInterval(-86400)], now: now)
    }

    /// Machines on the home network only, and no hub.
    private var nothing: RemoteAccess {
        let machines = Array(sampleFleetMachines(now: now).prefix(3)).map { machine in
            var machine = machine
            machine.addresses = ["172.20.40.\(machine.name.count):7447"]
            machine.address = machine.addresses.first
            return machine
        }
        return RemoteAccess(machines: machines, now: now)
    }

    @Test func drawsTheHubTabsRow() async throws {
        let row = RemoteAccessSection(access: working) {}.padding(16)
        try await shoot(row, size: CGSize(width: 390, height: 130), named: "remote-access-hub-iphone.png")
    }

    @Test func drawsThePageOnTheMac() async throws {
        try await shoot(RemoteAccessView(access: working) { _ in }, size: CGSize(width: 1040, height: 1250),
                        named: "remote-access-mac.png")
    }

    @Test func drawsThePageOnAnIPhone() async throws {
        try await shoot(RemoteAccessView(access: working) { _ in }, size: CGSize(width: 390, height: 1500),
                        named: "remote-access-iphone.png")
    }

    @Test func drawsNothingSetUpOnAnIPhone() async throws {
        try await shoot(RemoteAccessView(access: nothing) { _ in }, size: CGSize(width: 390, height: 3000),
                        named: "remote-access-none-iphone.png")
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
