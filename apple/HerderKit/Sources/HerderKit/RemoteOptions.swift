import Foundation
import Herder

/// The remote access options herder sets up itself (docs/remote-access.md), beside the routes
/// the owner runs: the WireGuard configs imported on this device, and what the hub offers and is
/// set to (`get_remote_access`). Each option reads as a `FleetReach.State`, like a route the
/// owner runs.
struct RemoteOptions: Equatable {
    /// How a route the hub runs in Cloudflare stands.
    struct Status: Equatable {
        /// "https://herder-relay.example.workers.dev", "wss://hub.example.com".
        var url: String
        var state: FleetReach.State
        /// Cloudflare's message, when it failed.
        var error: String?

        init(url: String, state: FleetReach.State, error: String? = nil) {
            self.url = url
            self.state = state
            self.error = error
        }

        init(_ status: RouteStatus) {
            url = status.url
            state = switch status.state {
            case .up: .working
            case .settingUp: .unseen
            case .failed: .wontWork
            }
            error = status.error
        }
    }

    /// A WireGuard config herder carries its own connections through, imported or the hub's.
    struct Tunnel: Hashable, Identifiable {
        let id: String
        let name: String
        /// The server's `host:port`.
        let endpoint: String
        let allowedIps: [String]
        /// Whether a connection runs through it now.
        let up: Bool
        /// The hub it enrolls with; nil for an imported config.
        let hub: HostId?

        init(id: String, name: String, endpoint: String, allowedIps: [String], up: Bool, hub: HostId? = nil) {
            self.id = id
            self.name = name
            self.endpoint = endpoint
            self.allowedIps = allowedIps
            self.up = up
            self.hub = hub
        }

        init(_ tunnel: WireGuardTunnel) {
            id = tunnel.id
            name = tunnel.name
            endpoint = tunnel.endpoint
            allowedIps = tunnel.allowedIps
            up = tunnel.up
            hub = switch tunnel.kind {
            case .hub(let hostId): hostId
            case .imported: nil
            }
        }
    }

    /// The hub asked; nil with no hub.
    var hub: HostId?
    /// Whether this device owns the hub, so it may change what the hub sets up.
    var owner = false
    /// Whether the hub answered `get_remote_access`; false while asking, and for a hub without
    /// it (an older herder) or offline.
    var answered = false
    /// Why the hub could not say: "needs herder 0.22 or newer on the hub", offline.
    var problem: String?
    /// herder's WireGuard on the hub: its endpoint while it is on.
    var hubWireGuard: String?
    /// Whether this device is enrolled with it.
    var enrolled = false
    /// The hub's WireGuard settings, for owners.
    var wireGuardPort: UInt16?
    var wireGuardEndpoint = ""
    /// The Cloudflare account the hub sets things up in, for owners; empty with none.
    var cloudflareAccount = ""
    /// The secret naming the API token, for owners.
    var cloudflareToken: SecretRef?
    /// Whether the owner turned the relay on, and the managed tunnel's hostname.
    var relayOn = false
    var tunnelHostname = ""
    /// The Durable Object relay and the managed tunnel, while set up or being set up.
    var relay: Status?
    var tunnel: Status?
    /// The WireGuard configs on this device.
    var tunnels: [Tunnel] = []

    init(hub: HostId? = nil, owner: Bool = false, tunnels: [Tunnel] = []) {
        self.hub = hub
        self.owner = owner
        self.tunnels = tunnels
    }

    /// The hub's answer folded in.
    mutating func read(_ offer: Herder.RemoteAccess) {
        answered = true
        problem = nil
        hubWireGuard = offer.wireguard?.endpoint
        enrolled = offer.wireguard?.enrolled ?? false
        relay = offer.relay.map(Status.init)
        tunnel = offer.tunnel.map(Status.init)
        let settings = offer.settings
        wireGuardPort = settings?.wireguard?.port
        wireGuardEndpoint = settings?.wireguard?.endpoint ?? ""
        let cloudflare = settings?.cloudflare
        cloudflareAccount = cloudflare?.accountId ?? ""
        cloudflareToken = cloudflare?.apiToken
        relayOn = cloudflare?.relay ?? false
        tunnelHostname = cloudflare?.tunnelHostname ?? ""
    }

    /// The configs imported on this device, not the hub's.
    var imported: [Tunnel] { tunnels.filter { $0.hub == nil } }

    /// Your VPN through herder: working while a connection runs through one of its configs, set
    /// up once one is imported.
    var ownVpnState: FleetReach.State {
        if imported.contains(where: \.up) { return .working }
        return imported.isEmpty ? .notSetUp : .unseen
    }

    /// herder's WireGuard on the hub, as this device uses it.
    var hubWireGuardState: FleetReach.State {
        guard hubWireGuard != nil else { return .notSetUp }
        if tunnels.contains(where: { $0.hub != nil && $0.up }) { return .working }
        return enrolled ? .unseen : .notSetUp
    }

    var relayState: FleetReach.State { relay?.state ?? .notSetUp }

    /// Cloudflare as one card: the worse of the tunnel and the relay when one failed, else the
    /// better.
    var cloudflareState: FleetReach.State {
        let states = [tunnelState, relayState]
        if states.contains(.wontWork) { return .wontWork }
        if states.contains(.working) { return .working }
        return states.contains(.unseen) ? .unseen : .notSetUp
    }

    /// The Cloudflare card's line: the tunnel's, then the relay's, those set up.
    var cloudflareLine: String {
        if let problem { return problem }
        let lines = [tunnel.map { _ in tunnelLine }, relay.map { _ in relayLine }].compactMap(\.self)
        guard lines.isEmpty else { return lines.joined(separator: "\n") }
        return owner ? "Not set up. With a Cloudflare API token, the hub runs a tunnel on a hostname of yours, or a relay on workers.dev for no public IP and no domain."
            : "Not set up. An owner of the hub sets it up."
    }

    /// Why the hub could not say what it offers, in a line.
    static func explain(_ error: any Error) -> String {
        if let error = error as? HerderError, error.needsNewerHerder {
            return "The hub’s herder is too old for this. Update it from Hub › Updates."
        }
        return describe(error)
    }
    var tunnelState: FleetReach.State { tunnel?.state ?? .notSetUp }

    /// Your VPN's line: "home-udm · 192.168.1.0/24 · up now".
    var ownVpnLine: String {
        guard !imported.isEmpty else {
            return "Not set up. Import your WireGuard server’s config (a UniFi router’s, say), and herder reaches home through it, beside any other VPN."
        }
        return imported.map { tunnel in
            ([tunnel.name] + (tunnel.allowedIps.isEmpty ? [] : [tunnel.allowedIps.joined(separator: ", ")])
                + [tunnel.up ? "up now" : "up only while used"]).joined(separator: " · ")
        }.joined(separator: "\n")
    }

    /// The hub's WireGuard line.
    var hubWireGuardLine: String {
        if let problem { return problem }
        guard let endpoint = hubWireGuard else {
            return owner ? "Off. One UDP port forwarded to the hub, and every device of yours reaches every machine through it."
                : "Off. An owner of the hub turns it on."
        }
        return enrolled ? "On, at \(endpoint) · this device uses it away from home"
            : "On, at \(endpoint) · not used on this device"
    }

    /// The managed tunnel's line.
    var tunnelLine: String {
        if let problem { return problem }
        guard let tunnel else {
            return owner ? "Not set up. The hub creates and runs a Cloudflare Tunnel on a hostname of yours."
                : "Not set up. An owner of the hub sets it up."
        }
        return Self.line(tunnel, up: "Up at")
    }

    /// The Durable Object relay's line.
    var relayLine: String {
        if let problem { return problem }
        guard let relay else {
            return owner ? "Not set up. For no public IP and no domain: the hub deploys a relay to your Cloudflare account."
                : "Not set up. An owner of the hub sets it up."
        }
        return Self.line(relay, up: "Relaying at")
    }

    private static func line(_ status: Status, up: String) -> String {
        let host = URL(string: status.url)?.host ?? status.url
        return switch status.state {
        case .working: "\(up) \(host)"
        case .wontWork: "Failed: \(status.error ?? "Cloudflare refused it")"
        default: "Setting up \(host)…"
        }
    }

    /// The settings that turn herder's WireGuard on at `endpoint`, port `port`, keeping the
    /// rest; nil turns it off.
    func settings(wireGuard: WireGuardSettings?) -> RemoteAccessSettings {
        RemoteAccessSettings(wireguard: wireGuard, cloudflare: cloudflare)
    }

    /// The settings with Cloudflare as given, keeping the WireGuard ones; nil removes it all.
    func settings(cloudflare: CloudflareSettings?) -> RemoteAccessSettings {
        RemoteAccessSettings(wireguard: wireGuardPort.map { WireGuardSettings(port: $0, endpoint: wireGuardEndpoint) },
                             cloudflare: cloudflare)
    }

    /// The Cloudflare settings as they stand; nil with none.
    var cloudflare: CloudflareSettings? {
        guard let cloudflareToken, !cloudflareAccount.isEmpty else { return nil }
        return CloudflareSettings(accountId: cloudflareAccount, apiToken: cloudflareToken, relay: relayOn,
                                  tunnelHostname: tunnelHostname.isEmpty ? nil : tunnelHostname)
    }

    /// The hub secret the Cloudflare API token is kept in, where `herder hub tunnel` puts it
    /// too, so either finds the other's.
    static let tokenSet = "cloudflare"
    static let tokenName = "CLOUDFLARE_API_TOKEN"
    static var tokenRef: SecretRef { "\(tokenSet)/\(tokenName)" }

    /// The hub's WireGuard port unless set otherwise, the protocol's `DEFAULT_WIREGUARD_PORT`:
    /// not WireGuard's usual 51820, which a cluster's network plugin takes on its nodes.
    static let defaultPort: UInt16 = 51900
}

/// A machine's addresses as routes, in the order they are tried, each named by what carries it,
/// with the user's choice for it: pinned (the only one tried), disabled (never tried) or in
/// the race.
struct MachineRoutes: Equatable, Identifiable {
    struct Entry: Hashable, Identifiable {
        var id: String { address }
        let address: String
        /// "Home network", "Tailscale", "Through herder-hub".
        let name: String
        let choice: RouteChoice
        /// Whether the connection in use goes over it.
        let inUse: Bool
    }

    var id: HostId { hostId }
    let hostId: HostId
    let name: String
    let entries: [Entry]
    /// The route in use, "Tailscale"; nil while not connected.
    let route: String?
    /// "38 ms": the latest round trip on the connection in use.
    let roundTrip: String?
    let signal: ConnectionSignal

    init(_ machine: Machine, among machines: [Machine]) {
        hostId = machine.hostId
        name = machine.name
        let connected = machine.connection == .connected ? machine.address : nil
        entries = machine.addresses.map { address in
            let choice: RouteChoice = if machine.pinnedAddress == address {
                .pinned
            } else if machine.disabledAddresses.contains(address) {
                .disabled
            } else {
                .auto
            }
            return Entry(address: address, name: MachineRoute(address: address, among: machines).name,
                         choice: choice, inUse: address == connected)
        }
        route = MachineRoute(machine, among: machines)?.name
        roundTrip = connected == nil ? nil : machine.quality.lastRttMs.map { "\($0) ms" }
        signal = ConnectionSignal(machine)
    }

    /// The row's line: "Tailscale · 38 ms", "Offline", "Pinned to Home network".
    var line: String {
        let said = [route ?? signal.word ?? "Offline", roundTrip].compactMap(\.self).joined(separator: " · ")
        guard let pinned = entries.first(where: { $0.choice == .pinned }) else { return said }
        return pinned.inUse ? "\(said) · pinned" : "\(said) · pinned to \(pinned.name)"
    }

    /// What a route's choice says beside it.
    static func label(_ choice: RouteChoice) -> String? {
        switch choice {
        case .auto: nil
        case .pinned: "Only this"
        case .disabled: "Never"
        }
    }
}

/// herder's own remote access options, as the Remote access page sets them up.
extension Fleet {
    /// Pins `address` (the only one dialled), disables it (never dialled), or puts it back in
    /// the race.
    func setRoute(_ hostId: HostId, address: String, to choice: RouteChoice) throws {
        try client.setRoute(hostId: hostId, address: address, choice: choice)
        update(client.machines())
    }

    /// The WireGuard configs on this device; none when they cannot be read.
    func wireGuardTunnels() -> [RemoteOptions.Tunnel] {
        ((try? client.wireguardTunnels()) ?? []).map(RemoteOptions.Tunnel.init)
    }

    /// Imports a WireGuard config's text as `name`; its private key stays on this device.
    func importWireGuard(name: String, config: String) throws {
        _ = try client.importWireguard(name: name.trimmingCharacters(in: .whitespaces), config: config)
        update(client.machines())
    }

    func removeWireGuard(_ id: String) throws {
        try client.removeWireguard(id: id)
        update(client.machines())
    }

    /// Enrolls this device with the hub's WireGuard, or removes it.
    func setHubWireGuard(_ hub: HostId, enabled: Bool) async throws {
        try await client.setHubWireguard(hostId: hub, enabled: enabled)
        update(client.machines())
    }

    /// What `hub` offers and, to an owner, how it is set.
    func remoteAccess(of hub: HostId) async throws -> Herder.RemoteAccess {
        guard case .remoteAccess(let offer) = try await client.send(hostId: hub, command: .getRemoteAccess) else {
            throw HerderError.Local(detail: "the hub did not say what it offers")
        }
        return offer
    }

    /// Sets what `hub` sets up; its answer, as applied.
    func setRemoteAccess(of hub: HostId, to settings: RemoteAccessSettings) async throws -> Herder.RemoteAccess {
        guard case .remoteAccess(let offer) = try await client.send(hostId: hub, command: .setRemoteAccess(settings: settings))
        else { throw HerderError.Local(detail: "the hub did not apply the settings") }
        return offer
    }

    /// Keeps `token` as the hub's Cloudflare API token, in its own set (created when the hub
    /// lacks it); the token is never read back.
    func putCloudflareToken(_ token: String, on hub: HostId) async throws {
        guard case .secrets(let sets, _) = try await client.send(hostId: hub, command: .listSecrets) else {
            throw HerderError.Local(detail: "the hub did not list its secrets")
        }
        if !sets.contains(where: { $0.name == RemoteOptions.tokenSet }) {
            _ = try await client.send(hostId: hub, command: .createSecretSet(set: RemoteOptions.tokenSet, assertion: nil))
        }
        _ = try await client.send(hostId: hub, command: .putSecret(
            set: RemoteOptions.tokenSet, name: RemoteOptions.tokenName,
            description: "Cloudflare API token the hub sets up its relay and tunnel with",
            value: token.trimmingCharacters(in: .whitespacesAndNewlines), assertion: nil))
    }
}

/// Whether `text`, such as a scanned QR code's, is a WireGuard config: a router's QR code for
/// a peer holds the `wg-quick` text itself.
func isWireGuardConfig(_ text: String) -> Bool {
    let lines = text.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces).lowercased() }
    return lines.contains("[interface]") && lines.contains("[peer]")
}
