import Herder
import SwiftUI

/// Reads `RemoteAccess` from the fleet: looks the hub's public name up, reads this device's
/// interfaces as connections change, and remembers what it saw working (`RemoteSightings`).
struct RemoteAccessReader<Content: View>: View {
    let fleet: Fleet
    @ViewBuilder var content: (RemoteAccess) -> Content
    @State private var resolved: [String: [String]] = [:]
    @State private var interfaces: [NetworkInterface] = []
    @AppStorage(RemoteSightings.key) private var sightings = ""

    var body: some View {
        let machines = fleet.machines
        let access = RemoteAccess(machines: machines, resolved: resolved, interfaces: interfaces,
                                  seen: RemoteSightings.decode(sightings))
        content(access)
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
}

/// Hub's Remote access: the fleet's reach in a line, each route's mark under it, and the page a
/// tap away.
struct RemoteAccessSection: View {
    let access: RemoteAccess
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

/// The Remote access page: a card per route, the hub first, each with its facts and the short
/// steps that set it up.
struct RemoteAccessPage: View {
    let fleet: Fleet
    @Environment(\.pages) private var pages

    var body: some View {
        RemoteAccessReader(fleet: fleet) { access in
            RemoteAccessView(access: access) { pages.open(.machine($0)) }
        }
        .navigationTitle("Remote access")
    }
}

/// What `RemoteAccessPage` draws. `openMachine` opens a machine's page, where its public
/// addresses and proxy headers are set.
struct RemoteAccessView: View {
    let access: RemoteAccess
    let openMachine: (HostId) -> Void

    static let intro = "herder runs no VPN of its own: your devices reach your machines over what you already have. Every connection is end-to-end encrypted to the machine, whatever carries it, and none of these is required."

    var body: some View {
        DetailPage(title: "Remote access", status: access.summary) {
            Text(Self.intro).font(.footnote).foregroundStyle(Theme.tertiary)
                .fixedSize(horizontal: false, vertical: true)
            RouteCard(title: "Through the hub", symbol: "point.3.connected.trianglepath.dotted", state: access.hub.state,
                      line: access.hubLine, options: SetupOption.hub(access)) {
                hubFacts
            }
            .accessibilityIdentifier("remote-access-hub")
            RouteCard(title: "Tailscale", symbol: "lock.shield", state: access.tailscaleState,
                      line: access.tailscaleLine, options: SetupOption.tailscale) {
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
            RouteCard(title: "VPN on your router", symbol: "house.and.flag", state: access.router.state,
                      line: access.routerLine, options: SetupOption.router(access)) {
                if access.router.state == .working {
                    RowDivider()
                    FactRow(label: "Home subnets") {
                        Text(access.router.subnets.joined(separator: ", ")).font(Theme.mono)
                    }
                }
            }
            .accessibilityIdentifier("remote-access-router")
        }
    }

    @ViewBuilder private var hubFacts: some View {
        let hub = access.hub
        if hub.name != nil {
            RowDivider()
            FactRow(label: "Public address") {
                Text(hub.host ?? "None yet").font(hub.host == nil ? Theme.body : Theme.mono)
            }
            if let address = hub.address, AddressHeaders.takesHeaders(address) {
                RowDivider()
                FactRow(label: "Front-door login") {
                    Text(AddressHeaders.summary(hub.frontDoor) ?? "None, which is fine")
                }
            }
            if hub.address != nil {
                RowDivider()
                FactRow(label: "Checked") { Text(access.checked) }
            }
            if !hub.relays.isEmpty {
                RowDivider()
                FactRow(label: "Relays to") { Text(Directory.list(hub.relays)) }
            }
            if let hostId = hub.hostId, let name = hub.name {
                RowDivider()
                HStack {
                    Spacer(minLength: 0)
                    SettingsButton(title: "Open \(name)") { openMachine(hostId) }.fixedSize()
                        .accessibilityIdentifier("remote-access-open-hub")
                }
                .padding(.horizontal, Theme.cellPadding)
                .padding(.vertical, 8)
            }
        }
    }
}

/// A route's card: its name over a box with its mark, state and line, its facts, and **How
/// to set it up**, open while the route does not work yet.
private struct RouteCard<Facts: View>: View {
    let title: String
    let symbol: String
    let state: RemoteAccess.State
    let line: String
    let options: [SetupOption]
    let facts: Facts
    @State private var showsSteps: Bool

    init(title: String, symbol: String, state: RemoteAccess.State, line: String, options: [SetupOption],
         @ViewBuilder facts: () -> Facts) {
        self.title = title
        self.symbol = symbol
        self.state = state
        self.line = line
        self.options = options
        self.facts = facts()
        _showsSteps = State(initialValue: state != .working)
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
    }
}

/// A label and its value in a route's card.
private struct FactRow<Value: View>: View {
    let label: String
    @ViewBuilder var value: Value

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text(label).font(.subheadline).foregroundStyle(Theme.secondary).lineLimit(1).fixedSize()
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

    /// Through the hub: a tunnel, a port forward or a reverse proxy, then its address once.
    static func hub(_ access: RemoteAccess) -> [SetupOption] {
        let name = access.hub.name ?? "the hub"
        let page = "**Public addresses** on \(name)’s page"
        var options = [
            SetupOption(title: "Cloudflare Tunnel", steps: [
                "Give \(name) a tunnel listener: `tunnel = \"127.0.0.1:7448\"` in its `daemon.toml` (a hub in Kubernetes may use its pod’s address), then restart it.",
                "Run `cloudflared tunnel create hub` and `cloudflared tunnel route dns hub hub.example.com`, with its ingress pointed at `http://127.0.0.1:7448`.",
                "Add `https://hub.example.com` under \(page). Every device picks it up when it next connects.",
                "Optional: put Cloudflare Access in front with a service token, and give each device its headers under **Proxy headers…** beside that address.",
            ]),
            SetupOption(title: "Port forward", steps: [
                "On your router, forward a TCP port to \(name)’s port: 7448 for a hub `herder hub install` set up.",
                "Give it a fixed public IP or a dynamic DNS name.",
                "Add it as `home.example.net:7448` under \(page).",
            ]),
            SetupOption(title: "Reverse proxy", steps: [
                "Pass TCP through to \(name) without ending TLS (an nginx `stream` block, HAProxy in `mode tcp`, a LoadBalancer Service), or end HTTPS with WebSockets on and pass to its tunnel listener.",
                "Add the proxy’s `host:port`, or its `https://` address, under \(page).",
            ]),
        ]
        if access.hub.name == nil {
            options.insert(SetupOption(title: "First, a hub", steps: [
                "A hub keeps session copies and relays to every machine that syncs to it, so it is the one machine to expose. Add one with **Add a Machine › Kubernetes**.",
            ]), at: 0)
        }
        return options
    }

    static let tailscale = [
        SetupOption(title: "Tailscale", steps: [
            "Install Tailscale on each machine and on this device, signed in to the same tailnet.",
            "A machine that listens on every interface lists its Tailscale address by itself. Otherwise add `name.tailnet.ts.net:7447` under its addresses.",
            "Keep Tailscale on, on this device, while you are away from home.",
        ]),
    ]

    /// A VPN server on the router, with a split config that routes only the home subnets.
    static func router(_ access: RemoteAccess) -> [SetupOption] {
        let subnets = access.homeSubnets.isEmpty ? "192.168.1.0/24" : access.homeSubnets.joined(separator: ", ")
        return [
            SetupOption(title: "WireGuard or OpenVPN on your router", steps: [
                "Turn on your router’s VPN server, such as WireGuard on a UniFi gateway (Settings › VPN › VPN Server).",
                "Make this device’s config split: `AllowedIPs = \(subnets)`, your home subnets only, not `0.0.0.0/0`, so the rest of its traffic does not go through home.",
                "Import it into the WireGuard or OpenVPN app here, and turn it on away from home (On Demand).",
                "Nothing to add in herder: your machines’ home addresses answer through it.",
            ]),
        ]
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
    let state: RemoteAccess.State

    private var tint: Color {
        switch state {
        case .working: Theme.success
        case .wontWork: Theme.accent
        case .unseen, .notSetUp: Theme.tertiary
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
    let state: RemoteAccess.State

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
            case .wontWork:
                Image(systemName: "exclamationmark.triangle.fill").font(.system(size: 9)).foregroundStyle(Theme.accent)
            }
        }
        .accessibilityHidden(true)
    }
}
