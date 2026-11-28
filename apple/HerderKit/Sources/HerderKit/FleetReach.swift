import Foundation
import Herder

/// How this fleet is reached from outside home, route by route, read from what the app already
/// has: the machines' addresses and connections, the hub's public address and its check
/// (`PublicCheck`), this device's own network interfaces, and the options herder sets up itself
/// (`RemoteOptions`). Each route is one the owner set up, or did not, which is a choice, never a
/// problem.
struct FleetReach: Equatable {
    /// How a route stands.
    enum State: Hashable {
        /// It answered from outside: now, or before (`RemoteSightings`).
        case working
        /// It is set up, and has not been seen working from outside yet.
        case unseen
        case notSetUp
        /// It is set up so it cannot work: a public name that resolves only to the LAN, or to
        /// nothing.
        case wontWork

        var label: String {
            switch self {
            case .working: "Working"
            case .unseen: "Set up, not seen working from outside yet"
            case .notSetUp: "Not set up"
            case .wontWork: "Won’t work as set up"
            }
        }
    }

    /// Through the hub, at its public address: the hub relays to every machine that syncs to it.
    struct Hub: Equatable {
        /// Nil with no hub.
        var name: String?
        var hostId: HostId?
        /// Its furthest-reaching public address, as saved; nil with none.
        var address: String?
        /// The headers this device sends a proxy in front of that address (P26.2).
        var frontDoor: [String] = []
        /// "Sign In…" or "Sign In Again…" while a login proxy in front of that address wants a
        /// browser sign-in (P28.7); nil otherwise.
        var signIn: String?
        /// Whether this device is signed in to that login proxy.
        var signedIn = false
        var check = PublicCheck.unchecked
        /// When this device last reached the hub through that address.
        var lastSeen: Date?
        /// The machines it relays to.
        var relays: [String] = []
        var state = State.notSetUp

        /// The public address's host: "hub.example.com".
        var host: String? { address.map(AddressScope.host(of:)) }

        /// The host to look up, when it is a name.
        var lookup: String? {
            guard let host, HostLookup.needsLookup(host) else { return nil }
            return host
        }
    }

    /// A machine with a Tailscale address.
    struct TailscaleMachine: Hashable, Identifiable {
        var id: HostId { hostId }
        let hostId: HostId
        let name: String
        /// Whether its connection is over Tailscale now.
        let answering: Bool
        /// When it last answered on Tailscale here.
        let lastSeen: Date?
    }

    /// A VPN on the home router, such as WireGuard on a UniFi gateway.
    struct Router: Equatable {
        /// The home subnets that answered through it: now, else when it last worked.
        var subnets: [String] = []
        /// When it last worked; nil while it works now, or never did.
        var lastSeen: Date?
        var state = State.notSetUp
    }

    var hub: Hub
    var tailscale: [TailscaleMachine]
    var tailscaleState: State
    var router: Router
    /// The /24s of the machines' private IPv4 addresses, the ones in use first: what a split VPN
    /// config routes home.
    var homeSubnets: [String]
    /// What this device saw working from outside just now, for `RemoteSightings` to remember.
    var sightings: [String]
    /// When each imported WireGuard config, by id, was last seen up on this device.
    var tunnelsSeen: [String: Date]
    /// The options herder sets up: an imported VPN config, the hub's WireGuard, Cloudflare.
    var options: RemoteOptions
    let now: Date

    /// The hub with a public address, else the first; nil with none.
    static func hub(among machines: [Machine]) -> Machine? {
        let hubs = machines.filter(\.isHub)
        return hubs.first(where: { $0.addresses.contains { AddressScope($0) == .anywhere } }) ?? hubs.first
    }

    /// `resolved` is what each name looked up resolved to (`HostLookup`), `interfaces` this
    /// device's (`NetworkInterface.current()`), `seen` when each route last worked
    /// (`RemoteSightings`).
    init(machines: [Machine], resolved: [String: [String]] = [:], interfaces: [NetworkInterface] = [],
         seen: [String: Date] = [:], options: RemoteOptions = RemoteOptions(), now: Date = .now) {
        self.now = now
        self.options = options
        var sightings: [String] = []

        if let pick = Self.hub(among: machines) {
            let address = pick.addresses.first { AddressScope($0) == .anywhere }
            let check = address.map { PublicCheck(address: $0, of: pick, resolved: resolved) } ?? .unchecked
            let key = address.map { "hub:\($0)" }
            let lastSeen = key.flatMap { seen[$0] }
            if check == .connected, let key { sightings.append(key) }
            let state: State = switch (address, check) {
            case (nil, _): .notSetUp
            case (_, .localOnly), (_, .unresolved): .wontWork
            case (_, .connected): .working
            default: lastSeen == nil ? .unseen : .working
            }
            let door = address.flatMap { FrontDoorPrompt.door(of: $0, in: pick) }
            let signIn = door.flatMap(FrontDoorPrompt.action)
            hub = Hub(name: pick.name, hostId: pick.hostId, address: address,
                      frontDoor: (address.flatMap { pick.addressHeaders[$0] } ?? []).sorted(),
                      signIn: signIn, signedIn: door != nil && signIn == nil,
                      check: check, lastSeen: lastSeen,
                      relays: pick.hosts.filter(\.relay).map(\.hostName).sorted { $0.lowercased() < $1.lowercased() },
                      state: state)
        } else {
            hub = Hub()
        }

        // Tailscale: every machine with an address in its ranges, and whether it answers there.
        var tailscale: [TailscaleMachine] = []
        for machine in machines {
            let answering = MachineRoute(machine, among: machines)?.kind == .tailscale
            guard answering || machine.addresses.contains(where: { AddressScope($0) == .vpn }) else { continue }
            let key = "tailscale:\(machine.hostId)"
            if answering { sightings.append(key) }
            tailscale.append(TailscaleMachine(hostId: machine.hostId, name: machine.name, answering: answering,
                                              lastSeen: seen[key]))
        }
        self.tailscale = tailscale.sorted { $0.name.lowercased() < $1.name.lowercased() }
        tailscaleState = if tailscale.isEmpty {
            .notSetUp
        } else if tailscale.contains(where: { $0.answering || $0.lastSeen != nil }) {
            .working
        } else {
            .unseen
        }

        // A router's VPN: a machine connected at a home address on none of this device's own
        // networks, while this device has a tunnel up that is not Tailscale's.
        let networks = interfaces.filter { !$0.isTunnel }
        let tunnelled = interfaces.contains { $0.isTunnel && !IPv4.isTailscale($0.address) }
        var throughRouter = Set<String>()
        if tunnelled {
            for machine in machines where machine.connection == .connected {
                guard let address = machine.address, let ip = IPv4.parse(AddressScope.host(of: address)),
                      IPv4.isHome(ip), !networks.contains(where: { $0.contains(ip) }) else { continue }
                throughRouter.insert(IPv4.subnet(ip))
            }
        }
        let remembered = seen.filter { $0.key.hasPrefix(Self.vpnKey) }
        if !throughRouter.isEmpty {
            let subnets = throughRouter.sorted()
            sightings += subnets.map { Self.vpnKey + $0 }
            router = Router(subnets: subnets, state: .working)
        } else if let last = remembered.values.max() {
            router = Router(subnets: remembered.keys.map { String($0.dropFirst(Self.vpnKey.count)) }.sorted(),
                            lastSeen: last, state: .working)
        } else {
            router = Router()
        }

        func subnets(_ addresses: [String]) -> [String] {
            Set(addresses.compactMap { IPv4.parse(AddressScope.host(of: $0)) }.filter(IPv4.isHome).map(IPv4.subnet)).sorted()
        }
        let inUse = subnets(machines.filter { $0.connection == .connected }.compactMap(\.address))
        homeSubnets = inUse.isEmpty ? subnets(machines.flatMap(\.addresses)) : inUse

        // Each imported WireGuard config up now, and when each last was.
        sightings += options.imported.filter(\.up).map { Self.tunnelKey + $0.id }
        tunnelsSeen = Dictionary(uniqueKeysWithValues: seen.filter { $0.key.hasPrefix(Self.tunnelKey) }
            .map { (String($0.key.dropFirst(Self.tunnelKey.count)), $0.value) })
        self.sightings = sightings
    }

    /// What `RemoteSightings` keys a router's VPN subnet by, before the subnet.
    static let vpnKey = "vpn:"
    /// What it keys an imported WireGuard config by, before its id.
    static let tunnelKey = "wg:"

    /// An imported config's status: "Up now", "Idle · last up 2h ago", or idle until used.
    func status(_ tunnel: RemoteOptions.Tunnel) -> String {
        if tunnel.up { return "Up now" }
        return tunnelsSeen[tunnel.id].map { "Idle · last up \(ago($0))" } ?? "Idle · comes up when herder needs it"
    }

    /// The fleet's reach in a line, the Hub tab's: "Reachable from anywhere through the hub"
    /// when all is well.
    var summary: String {
        switch hub.state {
        case .working: return "Reachable from anywhere through the hub"
        case .wontWork: return "The hub’s public address \(hub.host ?? "") doesn’t reach it from outside"
        default: break
        }
        var others: [String] = []
        if options.hubWireGuardState == .working { others.append("herder’s WireGuard") }
        if options.ownVpnState == .working { others.append("your VPN") }
        if tailscaleState == .working { others.append("Tailscale") }
        if router.state == .working { others.append("your router’s VPN") }
        if options.relayState == .working { others.append("the Cloudflare relay") }
        if !others.isEmpty { return "Reachable over \(Self.joined(others))" }
        if hub.state == .unseen { return "The hub has a public address, not seen working from outside yet" }
        if tailscaleState == .unseen { return "Machines have Tailscale addresses, not seen answering on them yet" }
        return "Reached at home only · set up access from anywhere"
    }

    /// The fleet's state as one mark: working when any route works, won't work when the hub's
    /// does not and nothing else does.
    var overall: State {
        if hub.state == .wontWork { return .wontWork }
        let states = [hub.state, tailscaleState, router.state, options.ownVpnState, options.hubWireGuardState,
                      options.relayState]
        if states.contains(.working) { return .working }
        return states.contains(.unseen) ? .unseen : .notSetUp
    }

    /// A route by its short name, with its state.
    struct RouteState: Hashable {
        let name: String
        let state: State
    }

    /// Each route by its short name, with its state: the Hub tab's row. herder's own options
    /// join it once set up, so a fleet that uses none of them reads as before.
    var routes: [RouteState] {
        let own = [RouteState(name: "Your VPN", state: options.ownVpnState),
                   RouteState(name: "WireGuard", state: options.hubWireGuardState),
                   RouteState(name: "Relay", state: options.relayState)]
        return [RouteState(name: "Hub", state: hub.state), RouteState(name: "Tailscale", state: tailscaleState),
                RouteState(name: "Router VPN", state: router.state)] + own.filter { $0.state != .notSetUp }
    }

    /// The best of `states`: working, then set up, then won't work, then not set up.
    static func best(_ states: [State]) -> State {
        [State.working, .unseen, .wontWork].first(where: { states.contains($0) }) ?? .notSetUp
    }

    /// "A", "A and B", "A, B and C".
    static func joined(_ names: [String]) -> String {
        guard let last = names.last, names.count > 1 else { return names.first ?? "" }
        return names.dropLast().joined(separator: ", ") + " and " + last
    }

    /// The hub's public address when a port forward or a proxy carries it, rather than a
    /// tunnel's `wss://` address: the state of the route through it.
    var directState: State {
        guard let address = hub.address, !AddressHeaders.takesHeaders(address) else { return .notSetUp }
        return hub.state
    }

    /// The port forward's line: "home.example.net · connected through it now".
    var directLine: String {
        guard directState != .notSetUp, let host = hub.host else {
            return "Not set up. Optional: a port forward or a proxy of yours to the hub, its address saved once."
        }
        return "\(host) · \(checked.prefix(1).lowercased() + String(checked.dropFirst()))"
    }

    /// The browser sign-in to a login proxy in front of the hub's tunnel (P28.7): working
    /// while signed in, set up while it asks for one.
    var frontDoorState: State {
        if hub.signedIn { return .working }
        return hub.signIn == nil ? .notSetUp : .unseen
    }

    var frontDoorLine: String {
        let host = hub.host ?? "the hub"
        if hub.signedIn { return "Signed in on this device to the login in front of \(host)" }
        if hub.signIn != nil { return "The login in front of \(host) asks you to sign in with a browser" }
        return "Not set up. Optional: a login such as Cloudflare Access in front of the hub’s tunnel, signed in to here with a browser."
    }

    /// What is known of the hub's public address.
    var checked: String {
        switch hub.check {
        case .connected: "Connected through it now"
        case .localOnly(let found): "Resolves only to your local network (\(found.joined(separator: ", ")))"
        case .unresolved: "Doesn’t resolve"
        case .unchecked: hub.lastSeen.map { "Worked from outside \(ago($0))" } ?? "Not seen working from outside yet"
        }
    }

    /// Tailscale's route in a line: "On 3 machines · 2 answer on it now".
    var tailscaleLine: String {
        guard !tailscale.isEmpty else { return "No machine has a Tailscale address" }
        let on = tailscale.count == 1 ? "On 1 machine" : "On \(tailscale.count) machines"
        let answering = tailscale.filter(\.answering).count
        switch answering {
        case 0: return on
        case 1: return "\(on) · 1 answers on it now"
        default: return "\(on) · \(answering) answer on it now"
        }
    }

    /// What a machine's row under Tailscale says.
    func label(_ machine: TailscaleMachine) -> String {
        if machine.answering { return "Answers on it now" }
        return machine.lastSeen.map { "Answered \(ago($0))" } ?? "Has a Tailscale address"
    }

    /// The router's VPN in a line.
    var routerLine: String {
        guard router.state == .working else {
            return "None seen yet. A VPN app here that reaches home is used by itself, with nothing to add in herder."
        }
        let subnets = router.subnets.joined(separator: ", ")
        return router.lastSeen.map { "Worked from outside home \(ago($0)), to \(subnets)" }
            ?? "Working from outside home now, to \(subnets)"
    }

    /// "just now", "2h ago".
    func ago(_ date: Date) -> String {
        let age = Timestamp.age(date, now: now)
        return age == "now" ? "just now" : "\(age) ago"
    }
}

/// When each route last worked from outside, on this device, so at home the page still says
/// the router's VPN worked yesterday. Kept in its defaults, as JSON of seconds since 1970 by
/// route: "hub:<address>", "tailscale:<host id>", "vpn:<subnet>", "wg:<imported config id>".
enum RemoteSightings {
    static let key = "remoteSightings"

    static func decode(_ json: String) -> [String: Date] {
        guard let seconds = try? JSONDecoder().decode([String: Double].self, from: Data(json.utf8)) else { return [:] }
        return seconds.mapValues { Date(timeIntervalSince1970: $0) }
    }

    /// `json` with each of `keys` seen at `now`.
    static func recording(_ keys: [String], at now: Date, in json: String) -> String {
        var seen = decode(json).mapValues(\.timeIntervalSince1970)
        for key in keys { seen[key] = now.timeIntervalSince1970 }
        guard let data = try? JSONEncoder().encode(seen) else { return json }
        return String(decoding: data, as: UTF8.self)
    }
}

/// An IPv4 network interface of this device that is up.
struct NetworkInterface: Hashable {
    /// "en0", "utun4".
    let name: String
    let address: UInt32
    let netmask: UInt32

    /// Whether it is a tunnel's, a VPN's, rather than a network this device is on.
    var isTunnel: Bool { Self.tunnels.contains { name.hasPrefix($0) } }
    private static let tunnels = ["utun", "ipsec", "ppp", "tun", "tap", "wg"]

    /// Whether `ip` is on its network.
    func contains(_ ip: UInt32) -> Bool { address & netmask == ip & netmask }

    /// This device's IPv4 interfaces that are up, loopback aside.
    static func current() -> [NetworkInterface] {
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0, let first = list else { return [] }
        defer { freeifaddrs(first) }
        var found: [NetworkInterface] = []
        var next: UnsafeMutablePointer<ifaddrs>? = first
        while let entry = next {
            next = entry.pointee.ifa_next
            let flags = Int32(bitPattern: entry.pointee.ifa_flags)
            guard flags & IFF_UP != 0, flags & IFF_LOOPBACK == 0, let address = entry.pointee.ifa_addr,
                  address.pointee.sa_family == sa_family_t(AF_INET), let netmask = entry.pointee.ifa_netmask
            else { continue }
            found.append(NetworkInterface(name: String(cString: entry.pointee.ifa_name), address: ipv4(address),
                                          netmask: ipv4(netmask)))
        }
        return found
    }

    /// The IPv4 address in `address`, in host byte order.
    private static func ipv4(_ address: UnsafeMutablePointer<sockaddr>) -> UInt32 {
        address.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { UInt32(bigEndian: $0.pointee.sin_addr.s_addr) }
    }
}

/// IPv4 addresses as numbers, in host byte order.
enum IPv4 {
    /// "172.20.40.200" as a number; nil for anything else.
    static func parse(_ text: String) -> UInt32? {
        let parts = text.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 4 else { return nil }
        var value: UInt32 = 0
        for part in parts {
            guard let octet = UInt8(part) else { return nil }
            value = value << 8 | UInt32(octet)
        }
        return value
    }

    static func format(_ ip: UInt32) -> String {
        "\(ip >> 24).\(ip >> 16 & 0xFF).\(ip >> 8 & 0xFF).\(ip & 0xFF)"
    }

    /// The /24 `ip` is in: "172.20.40.0/24".
    static func subnet(_ ip: UInt32) -> String { format(ip & 0xFFFF_FF00) + "/24" }

    /// Whether `ip` is in a range a home network uses: 10/8, 172.16/12, 192.168/16.
    static func isHome(_ ip: UInt32) -> Bool {
        ip >> 24 == 10 || ip >> 20 == 0xAC1 || ip >> 16 == 0xC0A8
    }

    /// Whether `ip` is in Tailscale's 100.64.0.0/10.
    static func isTailscale(_ ip: UInt32) -> Bool { ip >> 22 == 401 }
}
