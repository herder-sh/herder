import AuthenticationServices
import Herder
import SwiftUI

/// Reads `FleetReach` from the fleet: looks the hub's public name up, reads this device's
/// interfaces as connections change, and remembers what it saw working (`RemoteSightings`).
struct RemoteAccessReader<Content: View>: View {
    let fleet: Fleet
    @ViewBuilder var content: (FleetReach, RemoteActions) -> Content
    @State private var resolved: [String: [String]] = [:]
    @State private var interfaces: [NetworkInterface] = []
    @State private var offer: RemoteOptions?
    @State private var tunnels: [RemoteOptions.Tunnel] = []
    /// Bumped after a change, so the hub is asked again.
    @State private var changes = 0
    @AppStorage(RemoteSightings.key) private var sightings = ""

    var body: some View {
        let machines = fleet.machines
        let hub = FleetReach.hub(among: machines)
        var options = offer ?? RemoteOptions()
        options.hub = hub?.hostId
        options.owner = hub?.role == .owner
        options.tunnels = tunnels
        let access = FleetReach(machines: machines, resolved: resolved, interfaces: interfaces,
                                  seen: RemoteSightings.decode(sightings), options: options)
        return content(access, actions(hub: hub?.hostId))
            // Read this device's WireGuard configs again as connections change, and after each change.
            .task(id: "\(machines.map { $0.address ?? "" }) \(changes)") {
                tunnels = fleet.wireGuardTunnels()
            }
            // Ask the hub what it offers when it connects, and after each change.
            .task(id: "\(hub?.hostId ?? "") \(hub?.connection == .connected) \(changes)") {
                guard let hub, hub.connection == .connected else { return }
                var asked = RemoteOptions()
                do {
                    asked.read(try await fleet.remoteAccess(of: hub.hostId))
                } catch {
                    asked.problem = RemoteOptions.explain(error)
                }
                offer = asked
            }
            .task(id: access.hub.lookup) {
                guard let name = access.hub.lookup, resolved[name] == nil else { return }
                resolved[name] = await HostLookup.addresses(of: name)
            }
            // Read again as connections change: a VPN coming up is one.
            .task(id: machines.map { "\($0.hostId) \($0.address ?? "")" }) {
                interfaces = NetworkInterface.current()
            }
            .task(id: access.sightings) {
                guard !access.sightings.isEmpty else { return }
                sightings = RemoteSightings.recording(access.sightings, at: .now, in: sightings)
            }
    }

    /// What the page does, through the fleet; each change to the hub asks it again.
    private func actions(hub: HostId?) -> RemoteActions {
        let fleet = fleet
        let changed = { changes += 1 }
        return RemoteActions(
            setRoute: { try fleet.setRoute($0, address: $1, to: $2) },
            setAddresses: { try fleet.setAddresses($0, to: $1) },
            importConfig: { try fleet.importWireGuard(name: $0, config: $1); changed() },
            removeTunnel: { try fleet.removeWireGuard($0); changed() },
            connectThrough: { hostId, address in
                try await fleet.connect(hostId, through: address)
                changed()
            },
            useHubWireGuard: { on in
                guard let hub else { return }
                try await fleet.setHubWireGuard(hub, enabled: on)
                changed()
            },
            setSettings: { settings in
                guard let hub else { return }
                var applied = RemoteOptions()
                applied.read(try await fleet.setRemoteAccess(of: hub, to: settings))
                offer = applied
            },
            putToken: { token in
                guard let hub else { return }
                try await fleet.putCloudflareToken(token, on: hub)
            },
            signInToFrontDoor: { address, open in
                guard let hub else { return }
                try await fleet.signInToFrontDoor(hub, address: address, open: open)
            })
    }
}

/// Hub's Remote access: the fleet's reach in a line, each route's mark under it, and the page a
/// tap away.
struct RemoteAccessSection: View {
    let access: FleetReach
    let open: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: "Remote access")
            Button(action: open) {
                HStack(spacing: 12) {
                    RouteTile(symbol: "globe", state: access.overall)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(access.summary).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text)
                            .fixedSize(horizontal: false, vertical: true)
                        HStack(spacing: 12) {
                            ForEach(access.routes, id: \.self) { route in
                                HStack(spacing: 5) {
                                    RouteMark(state: route.state)
                                    Text(route.name).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                                }
                                .fixedSize()
                            }
                        }
                    }
                    Spacer(minLength: 8)
                    Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                }
                .padding(.horizontal, Theme.cellPadding)
                .padding(.vertical, 10)
                .frame(maxWidth: .infinity, minHeight: Theme.tallRowHeight, alignment: .leading)
                .background(.card, in: .rect(cornerRadius: Theme.corner))
                .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Remote access, \(access.summary)")
        }
        .accessibilityIdentifier("hub-remote-access")
    }
}

/// The Remote access page: how the fleet is reached now, in a line, then a tab per way in.
struct RemoteAccessPage: View {
    let fleet: Fleet
    @Environment(\.pages) private var pages

    var body: some View {
        RemoteAccessReader(fleet: fleet) { access, actions in
            RemoteAccessView(access: access, machines: fleet.machines.map { MachineRoutes($0, among: fleet.machines) },
                             actions: actions) { pages.open(.machine($0)) }
        }
        .navigationTitle("Remote access")
    }
}

/// A way in from outside home, the Remote access page's tabs.
enum RemoteTab: Hashable, CaseIterable {
    /// WireGuard configs of your own server, imported into herder.
    case ownVpn
    /// herder's WireGuard endpoint on the hub.
    case hubWireGuard
    /// The relay, the managed tunnel and a browser sign-in, in your Cloudflare account.
    case cloudflare
    /// Tailscale, another VPN on this device, a port forward: nothing to set up in herder.
    case others

    /// The tab's name; shorter on a phone, where four share its width.
    var title: String {
        #if os(iOS)
        switch self {
        case .ownVpn: "Your VPN"
        case .hubWireGuard: "Hub"
        case .cloudflare: "Cloudflare"
        case .others: "Others"
        }
        #else
        switch self {
        case .ownVpn: "Your VPN"
        case .hubWireGuard: "Hub WireGuard"
        case .cloudflare: "Cloudflare"
        case .others: "Tailscale & others"
        }
        #endif
    }

    /// How the tab's way in stands: its best route.
    func state(_ access: FleetReach) -> FleetReach.State {
        let options = access.options
        return switch self {
        case .ownVpn: options.ownVpnState
        case .hubWireGuard: options.hubWireGuardState
        case .cloudflare: FleetReach.best([options.tunnelState, options.relayState, access.frontDoorState])
        case .others: FleetReach.best([access.tailscaleState, access.router.state, access.directState])
        }
    }

    /// The tab the page opens on: the first way in that works, else the first set up (idle or
    /// not seen yet), else
    /// Your VPN.
    static func first(_ access: FleetReach) -> RemoteTab {
        allCases.first(where: { $0.state(access) == .working })
            ?? allCases.first(where: { [.unseen, .idle].contains($0.state(access)) }) ?? .ownVpn
    }
}

/// What `RemoteAccessPage` draws. `openMachine` opens a machine's page, where its public
/// addresses and proxy headers are set.
struct RemoteAccessView: View {
    let access: FleetReach
    /// Each machine's routes, for the route in use and the user's ranking.
    var machines: [MachineRoutes] = []
    var actions = RemoteActions()
    let openMachine: (HostId) -> Void
    @State private var tab: RemoteTab
    @State private var showsDetail: Bool

    init(access: FleetReach, machines: [MachineRoutes] = [], actions: RemoteActions = RemoteActions(),
         tab: RemoteTab? = nil, showsDetail: Bool = false, openMachine: @escaping (HostId) -> Void) {
        self.access = access
        self.machines = machines
        self.actions = actions
        self.openMachine = openMachine
        _tab = State(initialValue: tab ?? RemoteTab.first(access))
        _showsDetail = State(initialValue: showsDetail)
    }

    static let footnote = "Every connection is end-to-end encrypted to the machine, whatever carries it. None of these is required: use any, or none."

    var body: some View {
        DetailPage(title: "Remote access") {
            summary
            VStack(alignment: .leading, spacing: 16) {
                Picker("Way in", selection: $tab) {
                    ForEach(RemoteTab.allCases, id: \.self) { tab in
                        Text(tab.title).tag(tab)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .accessibilityIdentifier("remote-access-tabs")
                switch tab {
                case .ownVpn: ownVpn
                case .hubWireGuard: hubWireGuard
                case .cloudflare: cloudflare
                case .others: others
                }
            }
            Text(Self.footnote).font(.footnote).foregroundStyle(Theme.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// How the fleet is reached now, in a line, and every machine's route in another; a tap
    /// shows the hub's public address and each machine's routes, to rank or pin.
    private var summary: some View {
        SettingsGroup {
            Button {
                withAnimation(.easeInOut(duration: 0.2)) { showsDetail.toggle() }
            } label: {
                HStack(spacing: 12) {
                    RouteTile(symbol: "globe", state: access.overall)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(access.summary).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text)
                            .fixedSize(horizontal: false, vertical: true)
                        let line = MachineRoutes.line(machines)
                        if !line.isEmpty {
                            Text(line).font(.subheadline.monospacedDigit()).foregroundStyle(Theme.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    Spacer(minLength: 8)
                    Image(systemName: showsDetail ? "chevron.up" : "chevron.down")
                        .font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                }
                .padding(Theme.cellPadding)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("remote-access-summary")
            if showsDetail {
                hubFacts
                if !machines.isEmpty {
                    MachineRoutesList(machines: machines, actions: actions)
                }
            }
        }
    }

    @ViewBuilder private var hubFacts: some View {
        let hub = access.hub
        if let hostId = hub.hostId, let name = hub.name {
            RowDivider()
            FactRow(label: "Public address") {
                Text(hub.host ?? "None, which is fine").font(hub.host == nil ? Theme.body : Theme.mono)
            }
            if hub.address != nil {
                RowDivider()
                FactRow(label: "Checked") { Text(access.checked) }
            }
            if let address = hub.address, AddressHeaders.takesHeaders(address) {
                RowDivider()
                FactRow(label: "Proxy headers") {
                    Text(AddressHeaders.summary(hub.frontDoor) ?? "None, which is fine")
                }
            }
            if !hub.relays.isEmpty {
                RowDivider()
                FactRow(label: "Relays to") { Text(Directory.list(hub.relays)) }
            }
            RowDivider()
            HStack {
                Text("Public addresses and proxy headers are set on \(name)’s page.")
                    .font(.caption).foregroundStyle(Theme.tertiary).fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 8)
                SettingsButton(title: "Open \(name)") { openMachine(hostId) }.fixedSize()
                    .accessibilityIdentifier("remote-access-open-hub")
            }
            .padding(.horizontal, Theme.cellPadding)
            .padding(.vertical, 8)
        }
    }

    // MARK: The tabs

    private var ownVpn: some View {
        RouteCard(title: "WireGuard configs", symbol: "key.horizontal", state: access.options.ownVpnState,
                  line: access.options.ownVpnLine, options: SetupOption.ownVpn(access)) {
            OwnVpnFacts(access: access, actions: actions)
        }
        .accessibilityIdentifier("remote-access-own-vpn")
    }

    @ViewBuilder private var hubWireGuard: some View {
        let options = access.options
        if options.hub == nil {
            RouteCard(title: "herder’s WireGuard on the hub", symbol: "network.badge.shield.half.filled",
                      state: .notSetUp, line: SetupOption.needsHub, options: [SetupOption.hub]) { EmptyView() }
                .accessibilityIdentifier("remote-access-hub-wireguard")
        } else {
            RouteCard(title: "herder’s WireGuard on the hub", symbol: "network.badge.shield.half.filled",
                      state: options.hubWireGuardState, line: options.hubWireGuardLine,
                      options: SetupOption.hubWireGuard(access)) {
                HubWireGuardFacts(options: options, actions: actions)
            }
            .accessibilityIdentifier("remote-access-hub-wireguard")
        }
    }

    @ViewBuilder private var cloudflare: some View {
        let options = access.options
        if options.hub == nil {
            RouteCard(title: "Set up by the hub", symbol: "cloud", state: .notSetUp, line: SetupOption.needsHub,
                      options: [SetupOption.hub]) { EmptyView() }
                .accessibilityIdentifier("remote-access-cloudflare")
        } else {
            RouteCard(title: "Relay and tunnel, set up by the hub", symbol: "cloud", state: options.cloudflareState,
                      line: options.cloudflareLine, options: SetupOption.cloudflare(access)) {
                CloudflareFacts(options: options, actions: actions)
            }
            .accessibilityIdentifier("remote-access-cloudflare")
        }
        RouteCard(title: "Browser sign-in", symbol: "person.badge.key", state: access.frontDoorState,
                  line: access.frontDoorLine, options: SetupOption.frontDoor(access)) {
            FrontDoorFacts(hub: access.hub, actions: actions)
        }
        .accessibilityIdentifier("remote-access-front-door")
    }

    @ViewBuilder private var others: some View {
        Text("These carry herder by themselves once they reach your machines: nothing to set up here.")
            .font(.footnote).foregroundStyle(Theme.secondary).fixedSize(horizontal: false, vertical: true)
        RouteCard(title: "Tailscale", symbol: "lock.shield", state: access.tailscaleState,
                  line: access.tailscaleLine, options: SetupOption.tailscale, opensSteps: false) {
            ForEach(access.tailscale) { machine in
                RowDivider()
                FactRow(label: machine.name) {
                    HStack(spacing: 6) {
                        RouteMark(state: machine.answering || machine.lastSeen != nil ? .working : .unseen)
                        Text(access.label(machine))
                    }
                }
            }
        }
        .accessibilityIdentifier("remote-access-tailscale")
        RouteCard(title: "Another VPN on this device", symbol: "house.and.flag", state: access.router.state,
                  line: access.routerLine, options: []) {
            if access.router.state == .working {
                RowDivider()
                FactRow(label: "Home subnets") {
                    Text(access.router.subnets.joined(separator: ", ")).font(Theme.mono)
                }
            }
        }
        .accessibilityIdentifier("remote-access-router")
        RouteCard(title: "Port forward or proxy", symbol: "arrow.left.arrow.right", state: access.directState,
                  line: access.directLine, options: SetupOption.direct(access), opensSteps: false) { EmptyView() }
            .accessibilityIdentifier("remote-access-direct")
    }
}

/// The browser sign-in's facts: **Sign In…** while the login in front of the hub's tunnel asks
/// for one, opening it in a browser sheet that closes once the hub has the token.
struct FrontDoorFacts: View {
    let hub: FleetReach.Hub
    let actions: RemoteActions
    @State private var signingIn = false
    @State private var error: String?
    @Environment(\.webAuthenticationSession) private var webAuthenticationSession

    var body: some View {
        if let title = hub.signIn, let address = hub.address {
            RowDivider()
            HStack(spacing: 10) {
                if signingIn {
                    Text("Waiting for you to sign in in the browser…").font(.footnote).foregroundStyle(Theme.secondary)
                }
                Spacer(minLength: 0)
                SettingsButton(title: title, style: .primary) { signIn(address) }
                    .fixedSize()
                    .disabled(signingIn)
                    .accessibilityIdentifier("remote-access-front-door-sign-in")
            }
            .padding(.horizontal, Theme.cellPadding)
            .padding(.vertical, 8)
        }
        if let error {
            Text(error).font(.footnote).foregroundStyle(Theme.failure)
                .padding(.horizontal, Theme.cellPadding).padding(.bottom, 8)
        }
    }

    private func signIn(_ address: String) {
        signingIn = true
        error = nil
        let session = webAuthenticationSession
        Task {
            do {
                try await actions.signInToFrontDoor(address) { url in
                    // The proxy sends the browser back to the tunnel, never to herder, so the
                    // session ends when the sign-in does, cancelled.
                    _ = try? await session.authenticate(using: url, callbackURLScheme: "herder",
                                                        preferredBrowserSession: .ephemeral)
                }
            } catch {
                self.error = describe(error)
            }
            signingIn = false
        }
    }
}

/// A route's card: its name over a box with its mark, state and line, its facts, and **How
/// to set it up** when it has steps, open while the route does not work yet unless told not to.
struct RouteCard<Facts: View>: View {
    let title: String
    let symbol: String
    let state: FleetReach.State
    let line: String
    let options: [SetupOption]
    let facts: Facts
    @State private var showsSteps: Bool

    /// `opensSteps`: whether the steps start open while the route does not work.
    init(title: String, symbol: String, state: FleetReach.State, line: String, options: [SetupOption],
         opensSteps: Bool = true, @ViewBuilder facts: () -> Facts) {
        self.title = title
        self.symbol = symbol
        self.state = state
        self.line = line
        self.options = options
        self.facts = facts()
        _showsSteps = State(initialValue: opensSteps && state != .working)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: title)
            SettingsGroup {
                HStack(alignment: .top, spacing: 12) {
                    RouteTile(symbol: symbol, state: state)
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: 6) {
                            RouteMark(state: state)
                            Text(state.label).font(.caption.weight(.medium)).foregroundStyle(Theme.secondary)
                        }
                        Text(line).font(Theme.body).foregroundStyle(Theme.text)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Spacer(minLength: 0)
                }
                .padding(Theme.cellPadding)
                facts
                if !options.isEmpty {
                    steps
                }
            }
        }
    }

    @ViewBuilder private var steps: some View {
        RowDivider()
        Button {
            withAnimation(.easeInOut(duration: 0.2)) { showsSteps.toggle() }
        } label: {
            HStack {
                Text("How to set it up").font(.subheadline.weight(.medium)).foregroundStyle(Theme.action)
                Spacer(minLength: 8)
                Image(systemName: showsSteps ? "chevron.up" : "chevron.down")
                    .font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
            }
            .padding(.horizontal, Theme.cellPadding)
            .frame(maxWidth: .infinity, minHeight: Theme.rowHeight, alignment: .leading)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("setup-steps-toggle")
        if showsSteps {
            SetupSteps(options: options)
        }
    }
}

/// A label and its value in a route's card.
struct FactRow<Value: View>: View {
    let label: String
    @ViewBuilder var value: Value

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text(label).font(.subheadline).foregroundStyle(Theme.secondary).lineLimit(1).layoutPriority(1)
            Spacer(minLength: 8)
            value
                .font(.subheadline).foregroundStyle(Theme.text)
                .multilineTextAlignment(.trailing)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 10)
    }
}

/// One way to set a route up: its name and a few numbered steps, in Markdown.
struct SetupOption: Hashable, Identifiable {
    var id: String { title }
    let title: String
    let steps: [String]

    /// What a way in the hub runs says with no hub.
    static let needsHub = "Needs a hub. A hub keeps session copies and relays to every machine that syncs to it."

    static let hub = SetupOption(title: "First, a hub", steps: [
        "Add one with **Add a Machine › Kubernetes**, or turn a machine that is always on into one.",
    ])

    /// "**Public addresses** on herder-hub-0’s page".
    private static func addresses(_ access: FleetReach) -> String {
        "**Public addresses** on \(access.hub.name ?? "the hub")’s page"
    }

    /// Your own WireGuard server's config, imported once.
    static func ownVpn(_ access: FleetReach) -> [SetupOption] {
        let subnets = access.homeSubnets.isEmpty ? "" : " (`\(access.homeSubnets.joined(separator: ", "))`)"
        #if os(iOS)
        let bring = "Tap **Import a WireGuard Config…**, then **Scan QR Code…** and point it at the code your server shows, or choose its `.conf` file."
        #else
        let bring = "Click **Import a WireGuard Config…**, then **Choose File…** for its `.conf`, or paste the config’s text."
        #endif
        return [
            SetupOption(title: "Import your WireGuard server’s config", steps: [
                "On your WireGuard server, add a client for this device: on a UniFi gateway, **Settings › VPN › VPN Server**; on any other, the peer config it exports.",
                bring,
                "That’s all: when your machines’ home addresses\(subnets) don’t answer directly, herder reaches them through it. Only herder’s connections use it, so another VPN here stays on.",
            ]),
        ]
    }

    /// herder's WireGuard on the hub: one UDP port forwarded, then a switch per device.
    static func hubWireGuard(_ access: FleetReach) -> [SetupOption] {
        let name = access.hub.name ?? "the hub"
        let port = access.options.wireGuardPort ?? RemoteOptions.defaultPort
        return [
            SetupOption(title: "Turn it on", steps: [
                "An owner enters your router’s public name or IP (a dynamic DNS name is fine) and the port, then **Turn On**.",
                "On your router, forward UDP port `\(port)` to \(name). A hub in Kubernetes has it on its Service already.",
                "On each device, turn on **Use on this device**.",
            ]),
        ]
    }

    /// What the hub needs to set the relay or the tunnel up, and the tunnel made by hand.
    static func cloudflare(_ access: FleetReach) -> [SetupOption] {
        let name = access.hub.name ?? "the hub"
        return [
            SetupOption(title: "Set up by the hub", steps: [
                "In Cloudflare, **My Profile › API Tokens**, create a token with Workers Scripts, Cloudflare Tunnel and DNS edit.",
                "**Set Up…** above with your account ID and the token. Give a hostname on your zone for a tunnel, or turn on **Relay** for no public IP and no domain.",
                "Every device picks the new address up when it next connects.",
            ]),
            SetupOption(title: "Or run your own Cloudflare Tunnel", steps: [
                "Give \(name) a tunnel listener: `tunnel = \"127.0.0.1:7448\"` in its `daemon.toml`, then restart it.",
                "Run `cloudflared tunnel create hub` and `cloudflared tunnel route dns hub hub.example.com`, its ingress pointed at `http://127.0.0.1:7448`.",
                "Add `https://hub.example.com` under \(addresses(access)).",
            ]),
        ]
    }

    /// A login such as Cloudflare Access in front of the hub's tunnel.
    static func frontDoor(_ access: FleetReach) -> [SetupOption] {
        let host = access.hub.host.flatMap { AddressHeaders.takesHeaders(access.hub.address ?? "") ? $0 : nil }
            ?? "hub.example.com"
        return [
            SetupOption(title: "Cloudflare Access", steps: [
                "In Zero Trust, **Access › Applications › Add an application › Self-hosted**, for `\(host)`.",
                "Add a policy that **Allow**s the people who may sign in: their emails, a domain or a group.",
                "On each device, **Sign In…** shows here once the login asks for it.",
            ]),
        ]
    }

    static let tailscale = [
        SetupOption(title: "Tailscale", steps: [
            "Install Tailscale on each machine and on this device, signed in to the same tailnet.",
            "A machine that listens on every interface lists its Tailscale address by itself. Otherwise add `name.tailnet.ts.net:7447` under its addresses.",
        ]),
    ]

    /// A port forward or a reverse proxy to the hub, then its address once.
    static func direct(_ access: FleetReach) -> [SetupOption] {
        let name = access.hub.name ?? "the hub"
        var options = [
            SetupOption(title: "Port forward", steps: [
                "On your router, forward a TCP port to \(name)’s port: 7448 for a hub `herder hub install` set up.",
                "Give it a fixed public IP or a dynamic DNS name, and add it as `home.example.net:7448` under \(addresses(access)).",
            ]),
            SetupOption(title: "Reverse proxy", steps: [
                "Pass TCP through to \(name) without ending TLS (an nginx `stream` block, HAProxy in `mode tcp`, a LoadBalancer Service).",
                "Add the proxy’s `host:port` under \(addresses(access)).",
            ]),
        ]
        if access.hub.name == nil { options.insert(Self.hub, at: 0) }
        return options
    }
}

/// Each option's name and its numbered steps, commands in monospace.
private struct SetupSteps: View {
    let options: [SetupOption]

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(options) { option in
                VStack(alignment: .leading, spacing: 6) {
                    Text(option.title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    ForEach(Array(option.steps.enumerated()), id: \.offset) { index, step in
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Text("\(index + 1).").font(.caption.monospacedDigit()).foregroundStyle(Theme.tertiary)
                                .frame(width: 16, alignment: .trailing)
                            Text(LocalizedStringKey(step)).font(.footnote).foregroundStyle(Theme.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }
            }
        }
        .textSelection(.enabled)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, Theme.cellPadding)
        .padding(.bottom, Theme.cellPadding)
    }
}

/// A route's symbol on a tile, tinted green when it works, amber when it won't, grey otherwise.
private struct RouteTile: View {
    let symbol: String
    let state: FleetReach.State

    private var tint: Color {
        switch state {
        case .working: Theme.success
        case .wontWork: Theme.accent
        case .unseen, .notSetUp, .idle: Theme.tertiary
        }
    }

    var body: some View {
        Image(systemName: symbol)
            .font(.body)
            .foregroundStyle(tint)
            .frame(width: 32, height: 32)
            .background(tint.opacity(0.16), in: .rect(cornerRadius: 8))
            .accessibilityHidden(true)
    }
}

/// A route's state as a small mark: a green dot when it works, a dashed ring when set up and
/// unseen, a ring when not set up, an amber triangle only when it cannot work. Never red: a
/// route not set up is a choice.
struct RouteMark: View {
    let state: FleetReach.State

    var body: some View {
        Group {
            switch state {
            case .working:
                Circle().fill(Theme.success).frame(width: 8, height: 8)
            case .unseen:
                Circle().strokeBorder(Theme.tertiary, style: StrokeStyle(lineWidth: 1.5, dash: [2, 2]))
                    .frame(width: 8, height: 8)
            case .notSetUp:
                Circle().strokeBorder(Theme.tertiary, lineWidth: 1.5).frame(width: 8, height: 8)
            case .idle:
                Circle().fill(Theme.tertiary).frame(width: 8, height: 8)
            case .wontWork:
                Image(systemName: "exclamationmark.triangle.fill").font(.system(size: 9)).foregroundStyle(Theme.accent)
            }
        }
        .accessibilityHidden(true)
    }
}
