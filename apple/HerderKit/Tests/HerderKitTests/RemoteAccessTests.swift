import Foundation
import Herder
@testable import HerderKit
import Testing

/// `sampleFleetMachines` as the hub at home, public at hub.example.com through a tunnel behind
/// a front-door login: work-mbp and trash-can-01 sync to it and it relays to both.
func remoteFleet(now: Date = .now) -> [Machine] {
    var machines = sampleFleetMachines(now: now)
    machines[3].addresses = ["172.20.40.200:7448", "wss://hub.example.com"]
    machines[3].address = "172.20.40.200:7448"
    machines[3].addressHeaders = ["wss://hub.example.com": ["cf-access-client-secret", "cf-access-client-id"]]
    for index in machines[3].hosts.indices { machines[3].hosts[index].relay = true }
    return machines
}

/// An interface as `NetworkInterface.current()` reads one.
func interface(_ name: String, _ address: String, _ netmask: String = "255.255.255.0") -> NetworkInterface {
    NetworkInterface(name: name, address: IPv4.parse(address) ?? 0, netmask: IPv4.parse(netmask) ?? 0)
}

@MainActor struct RemoteAccessTests {
    private let now = Date(timeIntervalSince1970: 1_800_000_000)

    /// With nothing set up, every route says so quietly, and the summary offers to set one up.
    @Test func nothingSetUpIsQuiet() {
        let machines = Array(sampleFleetMachines(now: now).prefix(3)).map { machine in
            var machine = machine
            machine.address = "192.168.1.\(machine.name.count):7447"
            return machine
        }
        let access = FleetReach(machines: machines, now: now)
        #expect(access.hub == FleetReach.Hub())
        #expect(access.tailscaleState == .notSetUp && access.router.state == .notSetUp)
        #expect(access.overall == .notSetUp)
        #expect(access.summary == "Reached at home only · set up access from anywhere")
        #expect(access.hubLine == "No hub yet. A hub relays to every machine that syncs to it.")
        #expect(access.tailscaleLine == "No machine has a Tailscale address")
        #expect(access.routerLine == "Not set up. Optional, for your home subnets from anywhere.")
        #expect(access.routes.map(\.state) == [.notSetUp, .notSetUp, .notSetUp])
        // A split config routes the subnets the machines answer on.
        #expect(access.homeSubnets == ["192.168.1.0/24"])
        #expect(SetupOption.router(access)[0].steps[1].contains("`AllowedIPs = 192.168.1.0/24`"))
        #expect(SetupOption.hub(access).map(\.title) == ["First, a hub", "Cloudflare Tunnel", "Port forward", "Reverse proxy"])
    }

    /// Through the hub works while this device is connected at its public address, and is
    /// remembered; it names the front-door login and the machines it relays to.
    @Test func theHubWorksThroughItsPublicAddress() {
        var machines = remoteFleet(now: now)
        // At home: set up, not seen from outside yet.
        let home = FleetReach(machines: machines, now: now)
        #expect(home.hub.state == .unseen && home.hub.host == "hub.example.com")
        #expect(home.hub.frontDoor == ["cf-access-client-id", "cf-access-client-secret"])
        #expect(home.hub.relays == ["trash-can-01", "work-mbp"])
        #expect(home.checked == "Not seen working from outside yet")
        #expect(home.summary == "Reachable over Tailscale")
        #expect(home.sightings == ["tailscale:h-can"])

        // Away, through the tunnel.
        machines[3].address = "wss://hub.example.com"
        let away = FleetReach(machines: machines, now: now)
        #expect(away.hub.state == .working && away.checked == "Connected through it now")
        #expect(away.hubLine == "hub.example.com · connected through it now")
        #expect(away.summary == "Reachable from anywhere through the hub")
        #expect(away.sightings.contains("hub:wss://hub.example.com"))

        // Back home, it is remembered.
        machines[3].address = "172.20.40.200:7448"
        let seen = ["hub:wss://hub.example.com": now.addingTimeInterval(-2 * 3600)]
        let later = FleetReach(machines: machines, seen: seen, now: now)
        #expect(later.hub.state == .working && later.checked == "Worked from outside 2h ago")
        #expect(later.summary == "Reachable from anywhere through the hub")
    }

    /// A public name that resolves only to the LAN cannot work, and says so first.
    @Test func aHubAddressThatCannotWorkSaysSo() {
        var machines = remoteFleet(now: now)
        machines[3].addresses = ["172.20.40.200:7448", "home.example.net:7448"]
        let access = FleetReach(machines: machines, resolved: ["home.example.net": ["172.20.40.200"]], now: now)
        #expect(access.hub.state == .wontWork && access.overall == .wontWork)
        #expect(access.summary == "The hub’s public address home.example.net doesn’t reach it from outside")
        #expect(access.checked == "Resolves only to your local network (172.20.40.200)")
        #expect(access.hub.lookup == "home.example.net")
        // A hub with no public address is not set up.
        machines[3].addresses = ["172.20.40.200:7448"]
        let none = FleetReach(machines: machines, now: now)
        #expect(none.hub.state == .notSetUp && none.hubLine == "No public address yet")
    }

    /// Tailscale is read from the machines' addresses, and which of them answer on it now.
    @Test func tailscaleListsTheMachinesWithAnAddressOnIt() {
        var machines = remoteFleet(now: now)
        machines[0].addresses = ["127.0.0.1:7447", "100.101.1.2:7447"]
        let access = FleetReach(machines: machines, now: now)
        #expect(access.tailscale.map(\.name) == ["trash-can-01", "work-mbp"])
        #expect(access.tailscaleState == .working)
        #expect(access.tailscaleLine == "On 2 machines · 1 answers on it now")
        #expect(access.tailscale.map(access.label) == ["Answers on it now", "Has a Tailscale address"])
        let seen = FleetReach(machines: machines, seen: ["tailscale:h-mbp": now.addingTimeInterval(-3 * 86400)], now: now)
        #expect(seen.label(seen.tailscale[1]) == "Answered 3d ago")
        // With addresses but none answering yet, it is set up and unseen.
        machines[1].address = "10.0.0.9:7447"
        #expect(FleetReach(machines: machines, now: now).tailscaleState == .unseen)
    }

    /// A router's VPN shows when a machine answers at a home address on none of this device's
    /// networks, through a tunnel that is not Tailscale's; it is remembered for when at home.
    @Test func aRoutersVPNIsSeenFromAway() {
        let machines = remoteFleet(now: now)
        let hotel = interface("en0", "192.168.0.23")
        let wireguard = interface("utun4", "192.168.3.2", "255.255.255.255")
        let away = FleetReach(machines: machines, interfaces: [hotel, wireguard], now: now)
        #expect(away.router == FleetReach.Router(subnets: ["172.20.40.0/24"], state: .working))
        #expect(away.routerLine == "Working from outside home now, to 172.20.40.0/24")
        #expect(away.sightings.contains("vpn:172.20.40.0/24"))
        #expect(away.summary == "Reachable over Tailscale and your router’s VPN")

        // Tailscale's own tunnel is not a router's VPN, and at home the hub is on this network.
        let tailscale = interface("utun3", "100.101.2.3", "255.192.0.0")
        #expect(FleetReach(machines: machines, interfaces: [hotel, tailscale], now: now).router.state == .notSetUp)
        let home = interface("en0", "172.20.40.12")
        #expect(FleetReach(machines: machines, interfaces: [home, wireguard], now: now).router.state == .notSetUp)

        // Remembered at home.
        let seen = ["vpn:172.20.40.0/24": now.addingTimeInterval(-86400)]
        let later = FleetReach(machines: machines, interfaces: [home], seen: seen, now: now)
        #expect(later.router.state == .working)
        #expect(later.routerLine == "Worked from outside home 1d ago, to 172.20.40.0/24")
    }

    /// Steps stay vendor-neutral and name what this fleet has: its hub, its home subnets.
    @Test func stepsNameThisFleet() {
        var machines = remoteFleet(now: now)
        machines[0].addresses = ["172.20.10.5:7447"]
        machines[0].connection = .disconnected(error: "asleep")
        let access = FleetReach(machines: machines, now: now)
        #expect(access.homeSubnets == ["172.20.40.0/24"])
        for index in machines.indices { machines[index].connection = .disconnected(error: "away") }
        let away = FleetReach(machines: machines, now: now)
        #expect(away.homeSubnets == ["172.20.10.0/24", "172.20.40.0/24"])
        #expect(SetupOption.router(away)[0].steps[1].contains("`AllowedIPs = 172.20.10.0/24, 172.20.40.0/24`"))
        #expect(SetupOption.hub(away).map(\.title) == ["Cloudflare Tunnel", "Port forward", "Reverse proxy"])
        #expect(SetupOption.hub(away)[0].steps[2].contains("herder-hub-0’s page"))
    }

    @Test func sightingsRoundTrip() {
        let json = RemoteSightings.recording(["vpn:172.20.40.0/24"], at: now, in: "")
        let more = RemoteSightings.recording(["hub:wss://hub.example.com"], at: now.addingTimeInterval(60), in: json)
        #expect(RemoteSightings.decode(more) == ["vpn:172.20.40.0/24": now,
                                                 "hub:wss://hub.example.com": now.addingTimeInterval(60)])
        #expect(RemoteSightings.decode("not json").isEmpty)
    }

    @Test func ipv4Ranges() {
        #expect(IPv4.parse("172.20.40.200").map(IPv4.format) == "172.20.40.200")
        #expect(IPv4.parse("hub.lan") == nil && IPv4.parse("1.2.3") == nil && IPv4.parse("1.2.3.256") == nil)
        #expect(["10.1.2.3", "172.16.0.1", "172.31.255.1", "192.168.1.1"].compactMap(IPv4.parse).allSatisfy(IPv4.isHome))
        #expect(!["172.32.0.1", "100.64.0.1", "8.8.8.8", "127.0.0.1"].compactMap(IPv4.parse).contains(where: IPv4.isHome))
        #expect(IPv4.parse("100.64.0.1").map(IPv4.isTailscale) == true && IPv4.parse("100.128.0.1").map(IPv4.isTailscale) == false)
        #expect(interface("en0", "172.20.40.12").contains(IPv4.parse("172.20.40.200") ?? 0))
        #expect(interface("utun4", "10.8.0.2").isTunnel && !interface("en0", "10.8.0.2").isTunnel)
    }

    /// The interfaces are read from this device: loopback is left out.
    @Test func interfacesLeaveLoopbackOut() {
        #expect(!NetworkInterface.current().contains { $0.address >> 24 == 127 })
    }
}
