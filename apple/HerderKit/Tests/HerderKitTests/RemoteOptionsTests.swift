import Foundation
import Herder
@testable import HerderKit
import Testing

/// What the hub answers `get_remote_access` with, to its owner, with everything set up: its
/// WireGuard on at home.example.net, this device enrolled; the tunnel up; the relay failed.
func remoteOffer() -> Herder.RemoteAccess {
    let cloudflare = CloudflareSettings(accountId: "0123abcd", apiToken: RemoteOptions.tokenRef, relay: true,
                                        tunnelHostname: "hub.example.com")
    return Herder.RemoteAccess(
        settings: RemoteAccessSettings(wireguard: WireGuardSettings(port: 51820, endpoint: "home.example.net:51820"),
                                       cloudflare: cloudflare),
        wireguard: WireGuardOffer(endpoint: "home.example.net:51820", enrolled: true),
        relay: RouteStatus(url: "https://herder-relay.acme.workers.dev", state: .failed,
                           error: "the token lacks Workers Scripts edit"),
        tunnel: RouteStatus(url: "wss://hub.example.com", state: .up, error: nil))
}

/// A WireGuard config imported on this device, as client-core lists it.
func homeTunnel(up: Bool = false) -> RemoteOptions.Tunnel {
    RemoteOptions.Tunnel(id: "home-udm", name: "Home router", endpoint: "home.example.net:51821",
                         allowedIps: ["172.20.40.0/24"], up: up)
}

@MainActor struct RemoteOptionsTests {
    private let now = Date(timeIntervalSince1970: 1_800_000_000)

    /// With nothing of herder's set up, each option says so, and offers its set-up to an owner
    /// only; the Hub tab's row reads as before.
    @Test func nothingSetUpIsQuiet() {
        let options = RemoteOptions(hub: "h-hub", owner: false)
        #expect(options.ownVpnState == .notSetUp && options.hubWireGuardState == .notSetUp)
        #expect(options.cloudflareState == .notSetUp)
        #expect(options.hubWireGuardLine == "Off. An owner of the hub turns it on.")
        #expect(options.cloudflareLine == "Not set up. An owner of the hub sets it up.")
        let access = FleetReach(machines: remoteFleet(now: now), options: options, now: now)
        #expect(access.routes.map(\.name) == ["Hub", "Tailscale", "Router VPN"])
        #expect(RemoteOptions(hub: "h-hub", owner: true).hubWireGuardLine.hasPrefix("Off. One UDP port"))
    }

    /// The hub's answer sets each option's state and line, and keeps the owner's settings to
    /// change one without losing the rest.
    @Test func theHubsAnswerReadsAsEachOption() {
        var options = RemoteOptions(hub: "h-hub", owner: true)
        options.read(remoteOffer())
        #expect(options.answered && options.problem == nil)
        #expect(options.hubWireGuardState == .unseen)
        #expect(options.hubWireGuardLine == "On, at home.example.net:51820 · this device uses it away from home")
        #expect(options.tunnelState == .working && options.relayState == .wontWork)
        // One failed, so the card says so first.
        #expect(options.cloudflareState == .wontWork)
        #expect(options.cloudflareLine == "Up at hub.example.com\nFailed: the token lacks Workers Scripts edit")
        // Turning WireGuard off keeps Cloudflare; removing Cloudflare keeps WireGuard.
        let off = options.settings(wireGuard: nil)
        #expect(off.wireguard == nil && off.cloudflare?.tunnelHostname == "hub.example.com")
        let gone = options.settings(cloudflare: nil)
        #expect(gone.cloudflare == nil && gone.wireguard?.endpoint == "home.example.net:51820")
        // Through the hub's tunnel this device is using herder's WireGuard now.
        options.tunnels = [RemoteOptions.Tunnel(id: "hub", name: "herder-hub-0", endpoint: "home.example.net:51820",
                                                allowedIps: ["10.247.0.1/32"], up: true, hub: "h-hub")]
        #expect(options.hubWireGuardState == .working && options.imported.isEmpty)
    }

    /// An imported config is set up once imported, working while a connection runs through it,
    /// and joins the Hub tab's summary and row.
    @Test func anImportedConfigIsYourVPN() {
        var options = RemoteOptions(tunnels: [homeTunnel()])
        #expect(options.ownVpnState == .unseen)
        #expect(options.ownVpnLine == "Home router · 172.20.40.0/24 · up only while used")
        options.tunnels = [homeTunnel(up: true)]
        #expect(options.ownVpnState == .working)
        var machines = remoteFleet(now: now)
        machines[3].addresses = ["172.20.40.200:7448"]
        machines[1].addresses = ["172.20.40.9:7447"]
        machines[1].address = "172.20.40.9:7447"
        let access = FleetReach(machines: machines, options: options, now: now)
        #expect(access.summary == "Reachable over your VPN")
        #expect(access.routes.last == FleetReach.RouteState(name: "Your VPN", state: .working))
        #expect(access.overall == .working)
    }

    /// A hub too old to answer says so in a line, not as an error.
    @Test func aHubWithoutItSaysWhy() {
        let old = RemoteOptions.explain(HerderError.Rejected(info: ErrorInfo(
            code: .badRequest, message: "unknown variant `get_remote_access`, expected one of `list_sessions`")))
        #expect(old == "The hub’s herder is too old for this. Update it from Hub › Updates.")
        let refused = RemoteOptions.explain(HerderError.Rejected(info: ErrorInfo(code: .forbidden, message: "owners only")))
        #expect(refused == "owners only")
    }

    /// A machine's routes are named by what carries them, with the user's choice for each, and
    /// its line says the route in use, its round trip and a pin.
    @Test func aMachinesRoutesNameTheirCarrierAndChoice() {
        var machines = remoteFleet(now: now)
        var mbp = machines[0]
        mbp.addresses = ["127.0.0.1:7447", "wg://home-udm/172.20.40.5:7447", "100.101.1.2:7447", "hub://h-hub",
                         "relay+wss://herder-relay.acme.workers.dev"]
        mbp.address = "100.101.1.2:7447"
        mbp.disabledAddresses = ["hub://h-hub"]
        machines[0] = mbp
        let routes = MachineRoutes(mbp, among: machines)
        #expect(routes.entries.map(\.name) == ["Local network", "Your VPN", "Tailscale", "Through herder-hub-0",
                                               "Cloudflare relay"])
        #expect(routes.entries.map(\.choice) == [.auto, .auto, .auto, .disabled, .auto])
        #expect(routes.entries.map(\.inUse) == [false, false, true, false, false])
        #expect(routes.line == "Tailscale · 12 ms")
        mbp.pinnedAddress = "wg://home-udm/172.20.40.5:7447"
        #expect(MachineRoutes(mbp, among: machines).line == "Tailscale · 12 ms · pinned to Your VPN")
        mbp.connection = .disconnected(error: "asleep")
        #expect(MachineRoutes(mbp, among: machines).line == "Offline · pinned to Your VPN")
        #expect(MachineRoutesCard.moving(["a", "b", "c"], from: 2, by: -1) == ["a", "c", "b"])
        #expect(MachineRoutesCard.moving(["a", "b", "c"], from: 0, by: -1) == ["a", "b", "c"])
    }

    @Test func namesJoinAsASentence() {
        #expect(FleetReach.joined(["Tailscale"]) == "Tailscale")
        #expect(FleetReach.joined(["A", "B", "C"]) == "A, B and C")
    }
}
