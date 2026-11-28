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

    /// A router's QR code for a peer holds the config's text; anything else scanned is not one.
    @Test func aScannedWireGuardConfigIsTold() {
        let config = "[Interface]\nPrivateKey = yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=\nAddress = 192.168.3.2/32\n\n"
            + "[Peer]\r\nPublicKey = xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=\nEndpoint = home.example.net:51820\n"
        #expect(isWireGuardConfig(config))
        #expect(!isWireGuardConfig("herder://fleet?m=box"))
        #expect(!isWireGuardConfig("[Interface]\nPrivateKey = x"))
    }

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
        #expect(options.ownVpnState == .idle)
        #expect(options.ownVpnLine == "1 config · idle until herder needs one")
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
        #expect(MachineRoutesList.moving(["a", "b", "c"], from: 2, by: -1) == ["a", "c", "b"])
        #expect(MachineRoutesList.moving(["a", "b", "c"], from: 0, by: -1) == ["a", "b", "c"])
    }

    /// An imported config says whether it is up, and when it last was, from this device's
    /// memory; it routes "all addresses" for a router's full-tunnel export.
    @Test func anImportedConfigSaysWhenItWasLastUp() {
        let up = FleetReach(machines: [], options: RemoteOptions(tunnels: [homeTunnel(up: true)]), now: now)
        #expect(up.status(homeTunnel(up: true)) == "Working · up now")
        #expect(up.sightings == ["wg:home-udm"])
        // A machine with an address inside it, not connected: idle, by design.
        var machines = remoteFleet(now: now)
        machines[2].addresses = ["draugelis.lan:7447", "wg://home-udm/172.20.40.7:7447"]
        machines[2].connection = .disconnected(error: "away")
        let idle = FleetReach(machines: machines, seen: ["wg:home-udm": now.addingTimeInterval(-2 * 3600)],
                              options: RemoteOptions(tunnels: [homeTunnel()]), now: now)
        #expect(idle.status(homeTunnel()) == "Idle · last used 2h ago" && idle.sightings.isEmpty)
        #expect(idle.tryable(homeTunnel()) == nil)
        let never = FleetReach(machines: machines, options: RemoteOptions(tunnels: [homeTunnel()]), now: now)
        #expect(never.status(homeTunnel()) == "Idle · comes up when herder needs it")
        let unused = FleetReach(machines: [], options: RemoteOptions(tunnels: [homeTunnel()]), now: now)
        #expect(unused.status(homeTunnel()) == "Not used: no machine has a home address inside it")
        let all = RemoteOptions.Tunnel(id: "udm", name: "UniFi", endpoint: "home.example.net:51820",
                                       allowedIps: ["0.0.0.0/0"], up: false)
        #expect(all.reaches == "all addresses" && homeTunnel().reaches == "172.20.40.0/24")
    }

    /// A config whose machine stays connected over another route is never tried, as the route
    /// in use is kept; it says so, and offers to reconnect through it.
    @Test func anUnusedConfigSaysWhyAndOffersToTryIt() {
        var machines = remoteFleet(now: now)
        // trash-can-01 is connected over Tailscale, with an address through the config too.
        machines[1].addresses = ["trash-can-01.tail.ts.net:7447", "wg://home-udm/172.20.40.9:7447"]
        let access = FleetReach(machines: machines, options: RemoteOptions(tunnels: [homeTunnel()]), now: now)
        #expect(access.status(homeTunnel()) == "Not used: trash-can-01 is connected through Tailscale")
        #expect(access.tryable(homeTunnel())?.address == "wg://home-udm/172.20.40.9:7447")
        // Once its connection runs through the config, it says so.
        machines[1].address = "wg://home-udm/172.20.40.9:7447"
        let through = FleetReach(machines: machines, options: RemoteOptions(tunnels: [homeTunnel(up: true)]), now: now)
        #expect(through.status(homeTunnel(up: true)) == "Working · carries trash-can-01")
        #expect(through.tryable(homeTunnel(up: true)) == nil)
    }

    /// The summary's line says how many machines are connected, over what, and their round
    /// trips.
    @Test func theSummarySaysEachMachinesRoute() {
        var machines = remoteFleet(now: now)
        let all = machines.map { MachineRoutes($0, among: machines) }
        #expect(MachineRoutes.line(all).hasPrefix("All 4 connected · "))
        #expect(MachineRoutes.line(all).hasSuffix(" ms"))
        machines[0].connection = .disconnected(error: "asleep")
        #expect(MachineRoutes.line(machines.map { MachineRoutes($0, among: machines) }).hasPrefix("3 of 4 connected · "))
        for index in machines.indices { machines[index].connection = .disconnected(error: "away") }
        #expect(MachineRoutes.line(machines.map { MachineRoutes($0, among: machines) }) == "No machine connected right now")
        #expect(MachineRoutes.line([]) == "")
    }

    /// The page opens on the first way in that works, else the first set up, else Your VPN.
    @Test func thePageOpensOnAWayInInUse() {
        let none = FleetReach(machines: [], now: now)
        #expect(RemoteTab.first(none) == .ownVpn)
        var options = RemoteOptions(hub: "h-hub", owner: true)
        options.read(remoteOffer())
        let cloudflare = FleetReach(machines: [], options: options, now: now)
        #expect(RemoteTab.first(cloudflare) == .cloudflare)
        let vpn = FleetReach(machines: [], options: RemoteOptions(tunnels: [homeTunnel()]), now: now)
        #expect(RemoteTab.first(vpn) == .ownVpn && RemoteTab.ownVpn.state(vpn) == .idle)
        var machines = remoteFleet(now: now)
        machines[0].addresses = ["100.101.1.2:7447"]
        machines[0].address = "100.101.1.2:7447"
        #expect(RemoteTab.first(FleetReach(machines: machines, now: now)) == .others)
    }

    /// A browser sign-in to the login in front of the hub's tunnel is offered while it asks,
    /// and says so once signed in; a port forward is the hub's public address when not a tunnel.
    @Test func theHubsFrontDoorAndPortForwardReadAsRoutes() {
        var machines = remoteFleet(now: now)
        #expect(FleetReach(machines: machines, now: now).frontDoorState == .notSetUp)
        #expect(FleetReach(machines: machines, now: now).directState == .notSetUp)
        machines[3].frontDoors = [FrontDoor(address: "wss://hub.example.com", expiresAt: nil, signIn: true)]
        let asks = FleetReach(machines: machines, now: now)
        #expect(asks.hub.signIn == "Sign In…" && asks.frontDoorState == .unseen)
        machines[3].frontDoors = [FrontDoor(address: "wss://hub.example.com", expiresAt: nil, signIn: false)]
        let signedIn = FleetReach(machines: machines, now: now)
        #expect(signedIn.hub.signedIn && signedIn.frontDoorState == .working)
        machines[3].addresses = ["172.20.40.200:7448", "home.example.net:7448"]
        let direct = FleetReach(machines: machines, now: now)
        #expect(direct.directState == .unseen)
        #expect(direct.directLine == "home.example.net · not seen working from outside yet")
    }

    @Test func namesJoinAsASentence() {
        #expect(FleetReach.joined(["Tailscale"]) == "Tailscale")
        #expect(FleetReach.joined(["A", "B", "C"]) == "A, B and C")
    }
}
