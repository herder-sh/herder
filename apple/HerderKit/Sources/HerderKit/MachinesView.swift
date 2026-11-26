import Foundation
import Herder
import SwiftUI

/// How this device reaches a machine, as Setup says it: quietly, as a laptop asleep is expected.
enum MachineReach: Hashable {
    /// Connected, at one of the machine's addresses.
    case direct
    /// Connected through the hub of that name, as no address of its own answered.
    case through(String)
    case connecting
    /// Not connected here, while its hub hears from it: it is up, on a network this device is
    /// not on.
    case elsewhere
    case offline

    /// `hub` names the hub the connection goes through, when it goes through one.
    init(_ connection: ConnectionState, heardByHub: Bool, through hub: String? = nil) {
        switch connection {
        case .connected:
            if let hub { self = .through(hub) } else { self = .direct }
        case .connecting: self = .connecting
        case .disconnected: self = heardByHub ? .elsewhere : .offline
        }
    }

    var label: String {
        switch self {
        case .direct: "Direct"
        case .through(let hub): "Through \(hub)"
        case .connecting: "Connecting…"
        case .elsewhere: "On another network"
        case .offline: "Offline"
        }
    }
}

/// A machine as a row of Setup: its name, how this device reaches it, and what limits you
/// there. What diagnoses it (address, round trip, load, CLIs, accounts) is on its page.
struct MachineRow: Hashable, Identifiable {
    var id: HostId { hostId }
    let hostId: HostId
    let name: String
    /// Whether you are a member there, not an owner.
    let member: Bool
    /// Whether the daemon runs on this Mac.
    let local: Bool
    let reach: MachineReach
    let running: Int

    /// A row per machine that runs sessions, by name; hubs are not machines. `heard` are the
    /// hosts a connected hub hears from.
    static func rows(_ machines: [Machine], summaries: [MachineSummary], heard: Set<HostId> = [],
                     localNames: Set<String> = localNames) -> [MachineRow] {
        let hosts = machines.filter { !$0.isHub }
        let local = thisMac(hosts, names: localNames)
        return hosts.map { machine in
            MachineRow(hostId: machine.hostId, name: machine.name, member: machine.role == .member,
                       local: machine.hostId == local,
                       reach: MachineReach(machine.connection, heardByHub: heard.contains(machine.hostId),
                                           through: machine.address.flatMap { via($0, among: machines) }),
                       running: summaries.first { $0.hostId == machine.hostId }?.running ?? 0)
        }
        .sorted { $0.name.lowercased() < $1.name.lowercased() }
    }

    /// The names this Mac answers to, lowercased: loopback and its host name, as the kernel has
    /// it, which is what a daemon here names its machine. Read once, and with no lookup:
    /// `ProcessInfo.hostName` resolves the name over DNS and mDNS on the calling thread, and the
    /// sidebar reads this on every draw. Where no resolver answers, as on CI's Macs, each read
    /// held the main thread for seconds, and the mDNS query asked for Local Network access.
    static let localNames: Set<String> = {
        #if os(macOS)
        var buffer = [CChar](repeating: 0, count: Int(MAXHOSTNAMELEN) + 1)
        guard gethostname(&buffer, buffer.count - 1) == 0 else { return loopback }
        return names(host: String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self))
        #else
        return []
        #endif
    }()

    /// Loopback and `host`, lowercased, with and without ".local".
    static func names(host: String) -> Set<String> {
        let host = host.lowercased()
        let bare = host.hasSuffix(".local") ? String(host.dropLast(".local".count)) : host
        return loopback.union([host, bare, bare + ".local"])
    }

    /// The machine whose daemon runs on this Mac: of those reached at an address that names it,
    /// the one named after it, else the first paired. Several daemons can listen on loopback (a
    /// demo fleet, a tunnel), and only one of them is this Mac's.
    static func thisMac(_ machines: [Machine], names: Set<String>) -> HostId? {
        let local = machines.filter { isLocal($0.address, names: names) }
        return (local.first { names.contains($0.name.lowercased()) } ?? local.first)?.hostId
    }

    /// Whether `address`, "host:port" or "[v6]:port", names one of `names`.
    static func isLocal(_ address: String?, names: Set<String>) -> Bool {
        guard let address, !address.hasPrefix(hubRoute) else { return false }
        return names.contains(host(of: address).lowercased())
    }

    /// What the address of a route through a hub starts with, before the hub's host id.
    static let hubRoute = "hub://"

    /// The name of the hub `address` goes through, for "hub://<host id>", the route
    /// through a hub among `machines`: "a hub" for one this device no longer has. Nil for
    /// any other address.
    static func via(_ address: String, among machines: [Machine]) -> String? {
        guard address.hasPrefix(hubRoute) else { return nil }
        let hub = String(address.dropFirst(hubRoute.count))
        return machines.first { $0.hostId == hub }?.name ?? "a hub"
    }

    /// `address` as a person reads it: "through <hub>" for the route through a hub,
    /// else the address itself.
    static func label(_ address: String, among machines: [Machine]) -> String {
        via(address, among: machines).map { "through \($0)" } ?? address
    }

    /// The host of `address`, "host:port" or "[v6]:port", without its port.
    static func host(of address: String) -> String {
        if address.hasPrefix("["), let close = address.firstIndex(of: "]") {
            String(address[address.index(after: address.startIndex)..<close])
        } else {
            String(address.split(separator: ":").first ?? "")
        }
    }

    /// What loopback is called.
    static let loopback: Set<String> = ["localhost", "127.0.0.1", "::1"]
}

/// Where an address can be reached from, by what it names: nothing here knows how it is
/// exposed (a port forward, a reverse proxy, a tunnel, a VPN), only how far it reaches.
enum AddressScope: Comparable, Hashable {
    /// Loopback, a private range, or a name only a local network resolves.
    case network
    /// A VPN's range or name: Tailscale's 100.64.0.0/10, fd7a:115c:a1e0::/48 and `*.ts.net`.
    case vpn
    /// A public name or address.
    case anywhere

    /// The host of `address` without its scheme, port and path: `host:port`, `[v6]:port`, or
    /// a `wss://host[:port][/path]` URL.
    static func host(of address: String) -> String {
        var rest = Substring(address)
        if let scheme = rest.range(of: "://") { rest = rest[scheme.upperBound...] }
        return MachineRow.host(of: String(rest.prefix { $0 != "/" })).lowercased()
    }

    /// The ends of names only a local network resolves.
    private static let localSuffixes = [".local", ".lan", ".home", ".internal", ".home.arpa"]

    init(_ address: String) {
        let host = Self.host(of: address)
        let octets = host.split(separator: ".", omittingEmptySubsequences: false).map { UInt8($0) }
        if octets.count == 4, let first = octets[0], let second = octets[1], !octets.contains(nil) {
            switch (first, second) {
            case (10, _), (127, _), (192, 168), (169, 254), (172, 16...31): self = .network
            case (100, 64...127): self = .vpn
            default: self = .anywhere
            }
        } else if host.contains(":") {
            // An IPv6 address: Tailscale's range, then loopback, unique local and link local.
            if host.hasPrefix("fd7a:115c:a1e0:") {
                self = .vpn
            } else if host == "::1" || host.hasPrefix("fc") || host.hasPrefix("fd") || host.hasPrefix("fe80:") {
                self = .network
            } else {
                self = .anywhere
            }
        } else if host.hasSuffix(".ts.net") {
            self = .vpn
        } else if !host.contains(".") || Self.localSuffixes.contains(where: { host.hasSuffix($0) }) {
            self = .network
        } else {
            self = .anywhere
        }
    }
}

extension Machine {
    /// Whether it is a hub, which keeps its machines' sessions rather than running any.
    var isHub: Bool { !hosts.isEmpty || hub != nil }
}

/// Setup's structure: each hub with the machines that sync to it, then the machines that sync
/// to none.
struct SetupStructure: Equatable {
    struct Hub: Hashable, Identifiable {
        /// "paired.h-hub", or "unpaired." and its fingerprint.
        let id: String
        let name: String
        /// Its page; nil for a hub this device has not signed in to.
        var hostId: HostId?
        /// What Sign In adds, for a hub this device has not signed in to.
        var unpaired: UnpairedHub?
        /// This device's connection to it; nil when it has none to have.
        var connection: ConnectionState?
        /// How far its furthest-reaching address reaches, and that address's host.
        var scope = AddressScope.network
        var address: String?
        /// The sessions it keeps a copy of.
        var sessions = 0
        let machines: [MachineRow]

        /// "Reachable from anywhere, at hub.example.com": how far it reaches and through which
        /// address, however that address is exposed.
        var reachability: String {
            switch (scope, address) {
            case (.anywhere, let address?): "Reachable from anywhere, at \(address)"
            case (.vpn, let address?): "Reachable over your VPN, at \(address)"
            default: "Reachable on its network only"
            }
        }

        /// "Keeps copies of 7 sessions, your secrets and provider logins".
        var keeps: String {
            switch sessions {
            case 0: "Keeps session copies, secrets and provider logins"
            case 1: "Keeps a copy of 1 session, your secrets and provider logins"
            default: "Keeps copies of \(sessions) sessions, your secrets and provider logins"
            }
        }

        /// "work-mbp and trash-can-01 sync to it. Sign in to see it here.", for a hub this
        /// device has not signed in to.
        var invitation: String? {
            guard let unpaired else { return nil }
            let sync = unpaired.hosts.count == 1 ? "syncs" : "sync"
            return "\(Directory.list(unpaired.hosts)) \(sync) to it. Sign in to see it here."
        }
    }

    let hubs: [Hub]
    /// The machines that sync to no hub.
    let others: [MachineRow]

    var machines: Int { hubs.reduce(others.count) { $0 + $1.machines.count } }

    /// `unpaired` are the hubs the machines sync to that this device has not signed in to,
    /// which name their machines.
    init(machines: [Machine], summaries: [MachineSummary], unpaired: [UnpairedHub] = [],
         localNames: Set<String> = MachineRow.localNames) {
        let paired = machines.filter(\.isHub)
        // Only a hub connected here says who it hears from now.
        let heard = Set(paired.filter { $0.connection == .connected }.flatMap { $0.hosts.filter(\.online).map(\.hostId) })
        var rows = MachineRow.rows(machines, summaries: summaries, heard: heard, localNames: localNames)
        /// Takes the rows that match out of `rows`: a machine hangs under one hub.
        func take(_ matches: (MachineRow) -> Bool) -> [MachineRow] {
            let taken = rows.filter(matches)
            rows.removeAll(where: matches)
            return taken
        }
        let signedIn: [Hub] = paired.map { hub in
            let hosts = Set(hub.hosts.map(\.hostId))
            // Of the addresses that reach furthest, the one preferred.
            let scope = hub.addresses.map(AddressScope.init).max() ?? .network
            let furthest = hub.addresses.first { AddressScope($0) == scope }
            return Hub(id: "paired.\(hub.hostId)", name: hub.name, hostId: hub.hostId, connection: hub.connection,
                       scope: scope, address: furthest.map(AddressScope.host(of:)), sessions: hub.sessions.count,
                       machines: take { hosts.contains($0.hostId) })
        }
        let notSignedIn: [Hub] = unpaired.map { hub in
            Hub(id: "unpaired.\(hub.fingerprint)", name: hub.name, unpaired: hub,
                machines: take { hub.hosts.contains($0.name) })
        }
        hubs = signedIn + notSignedIn
        self.others = rows
    }
}

/// Setup's centre: each hub as a card with the machines that sync to it hanging under it, then
/// the machines that sync to none. A card or a row opens its page; a hub this device has not
/// signed in to offers Sign In.
struct MachinesView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    @State private var unpaired: [UnpairedHub] = []
    @Environment(\.pages) private var pages

    var body: some View {
        let structure = SetupStructure(machines: fleet.liveMachines, summaries: fleet.lists.machines, unpaired: unpaired)
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: structure.hubs.isEmpty ? "Machines" : "Hub and machines", count: structure.machines)
            ForEach(structure.hubs) { hub in
                VStack(spacing: 0) {
                    HubCard(hub: hub, open: { if let hostId = hub.hostId { pages.open(.hub(hostId)) } },
                            signIn: { sheet = .addMachine(hub: hub.unpaired) })
                    ForEach(Array(hub.machines.enumerated()), id: \.element.id) { index, row in
                        MachineCard(row: row) { pages.open(.machine(row.hostId)) }
                            .padding(.top, Self.gap)
                            .padding(.leading, Self.indent)
                            .background(alignment: .leading) {
                                HubRail(last: index == hub.machines.count - 1, gap: Self.gap)
                                    .stroke(Theme.tertiary, lineWidth: 1)
                                    .frame(width: Self.indent)
                            }
                    }
                }
            }
            if !structure.others.isEmpty {
                if !structure.hubs.isEmpty {
                    SectionHeading(title: "Other machines", count: structure.others.count).padding(.top, 12)
                }
                VStack(spacing: Self.gap) {
                    ForEach(structure.others) { row in
                        MachineCard(row: row) { pages.open(.machine(row.hostId)) }
                    }
                }
            }
            if structure.hubs.isEmpty {
                noHub
            } else if !structure.others.isEmpty {
                Text("These don’t sync to the hub. An owner turns it on from the machine’s page.")
                    .font(.footnote).foregroundStyle(Theme.tertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        // Asked again as machines connect, and as a hub gets signed in to.
        .task(id: fleet.machines.filter { $0.connection == .connected }.map(\.hostId)) {
            unpaired = await fleet.unpairedHubs()
        }
    }

    /// The space between two rows.
    private static let gap: CGFloat = 6
    /// How far the machines under a hub are set in, which is the rail's room.
    private static let indent: CGFloat = 24

    /// What a hub would add, in a line, and the way to one.
    private var noHub: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("No hub yet. A hub keeps session copies, secrets and provider logins, and your machines sync to it.")
                .font(.footnote).foregroundStyle(Theme.tertiary)
                .fixedSize(horizontal: false, vertical: true)
            Button("Add a hub…") { sheet = .addMachine(.kubernetes) }
                .buttonStyle(.plain)
                .font(.footnote.weight(.medium))
                .foregroundStyle(Theme.action)
                .hitTarget()
                .accessibilityIdentifier("add-hub")
        }
    }

    /// How many of the machines that run sessions are connected: "2 of 3 machines connected".
    static func connections(_ machines: [MachineSummary]) -> String {
        let hosts = machines.filter { !$0.hub }
        let connected = hosts.filter(\.connected).count
        if hosts.isEmpty { return "No machines yet" }
        if connected < hosts.count { return "\(connected) of \(hosts.count) machines connected" }
        return hosts.count == 1 ? "1 machine connected" : "All \(hosts.count) machines connected"
    }

    /// A connection's state in a word or two: "Connected", "Connecting…", "Offline".
    static func status(_ connection: ConnectionState) -> String {
        switch connection {
        case .connected: "Connected"
        case .connecting: "Connecting…"
        case .disconnected: "Offline"
        }
    }
}

/// The hub: its name, this device's connection to it, how far it is reached and at which
/// address, and what it keeps. It opens its page; one this device has not signed in to says who syncs to it
/// and offers Sign In.
private struct HubCard: View {
    let hub: SetupStructure.Hub
    let open: () -> Void
    let signIn: () -> Void

    var body: some View {
        Group {
            if hub.hostId == nil {
                card
            } else {
                Button(action: open) { card }.buttonStyle(.plain)
            }
        }
        .accessibilityIdentifier("machines.\(hub.id)")
    }

    private var card: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 12) {
                Image(systemName: "point.3.connected.trianglepath.dotted")
                    .font(.body)
                    .foregroundStyle(Theme.action)
                    .frame(width: 32, height: 32)
                    .background(Theme.action.opacity(0.16), in: .rect(cornerRadius: 8))
                VStack(alignment: .leading, spacing: 3) {
                    Text(hub.name).font(Theme.body.weight(.semibold)).foregroundStyle(Theme.text).lineLimit(1)
                    HStack(spacing: 6) {
                        if let connection = hub.connection {
                            ReachMark(reach: MachineReach(connection, heardByHub: false))
                            Text("Hub · \(MachinesView.status(connection))")
                        } else {
                            Text("Hub · not on this device yet")
                        }
                    }
                    .font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                }
                Spacer(minLength: 8)
                if hub.hostId == nil {
                    SettingsButton(title: "Sign In", style: .primary, action: signIn).fixedSize()
                } else {
                    Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                }
            }
            VStack(alignment: .leading, spacing: 6) {
                if let invitation = hub.invitation {
                    fact("person.badge.key", invitation)
                } else {
                    fact(hub.scope == .anywhere ? "globe" : hub.scope == .vpn ? "lock.shield" : "house", hub.reachability)
                    fact("tray.full", hub.keeps)
                }
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
        .contentShape(.rect)
    }

    private func fact(_ symbol: String, _ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: symbol).font(.caption).foregroundStyle(Theme.tertiary).frame(width: 16)
            Text(text).font(.caption).foregroundStyle(Theme.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// A machine's row: its name, "this Mac" and "Member" where they apply, how this device
/// reaches it, and how many sessions run there, when any do. It opens the machine's page.
private struct MachineCard: View {
    let row: MachineRow
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 8) {
                        Text(row.name).font(Theme.body.weight(.medium)).foregroundStyle(Theme.text).lineLimit(1)
                        if row.local { Pill(text: "this Mac").fixedSize() }
                        if row.member { Pill(text: "Member").fixedSize() }
                    }
                    HStack(spacing: 6) {
                        ReachMark(reach: row.reach)
                        Text(row.reach.label).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                    }
                }
                Spacer(minLength: 8)
                if row.running > 0 {
                    HStack(spacing: 5) {
                        Circle().fill(Theme.running).frame(width: 6, height: 6)
                        Text("\(row.running) running").font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                    }
                    .fixedSize()
                }
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
            }
            .padding(.horizontal, Theme.cellPadding)
            .padding(.vertical, 9)
            .frame(maxWidth: .infinity, minHeight: Theme.tallRowHeight, alignment: .leading)
            .background(.card, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(row.name), \(row.reach.label)")
        .accessibilityIdentifier("machines.\(row.hostId)")
    }
}

/// The rail the machines that sync to a hub hang from: down from the hub's card, with a stub
/// to this row, where it ends on the last one.
struct HubRail: Shape {
    let last: Bool
    /// How far under the rail's top the row starts.
    let gap: CGFloat

    func path(in rect: CGRect) -> Path {
        let x = rect.midX
        let y = gap + (rect.height - gap) / 2
        let bend = min(6, rect.maxX - x)
        var path = Path()
        path.move(to: CGPoint(x: x, y: rect.minY))
        path.addLine(to: CGPoint(x: x, y: y - bend))
        path.addQuadCurve(to: CGPoint(x: x + bend, y: y), control: CGPoint(x: x, y: y))
        path.addLine(to: CGPoint(x: rect.maxX, y: y))
        if !last {
            path.move(to: CGPoint(x: x, y: y - bend))
            path.addLine(to: CGPoint(x: x, y: rect.maxY))
        }
        return path
    }
}

/// How a machine is reached, as a small mark: green when direct, a dashed ring while
/// connecting, a ring when it is elsewhere or offline. Never red: a machine away is expected.
struct ReachMark: View {
    let reach: MachineReach

    var body: some View {
        Group {
            switch reach {
            case .direct, .through:
                Circle().fill(Theme.success)
            case .connecting:
                Circle().strokeBorder(Theme.waiting, style: StrokeStyle(lineWidth: 1.5, dash: [2, 2]))
            case .elsewhere, .offline:
                Circle().strokeBorder(Theme.tertiary, lineWidth: 1.5)
            }
        }
        .frame(width: 8, height: 8)
        .accessibilityHidden(true)
    }
}

/// A machine's connection state as a mark: filled when connected, a dashed ring while
/// connecting, a crossed circle when the connection failed.
struct ConnectionMark: View {
    let state: ConnectionState

    var body: some View {
        Group {
            switch state {
            case .connected:
                Circle().fill(Theme.success).frame(width: 7, height: 7)
            case .connecting:
                Circle().strokeBorder(Theme.waiting, style: StrokeStyle(lineWidth: 1.5, dash: [2, 2]))
                    .frame(width: 9, height: 9)
            case .disconnected:
                Circle().fill(Theme.failure).frame(width: 10, height: 10)
                    .overlay(Image(systemName: "xmark").font(.system(size: 5, weight: .heavy))
                        .foregroundStyle(.ground))
            }
        }
        .accessibilityLabel(state.label)
    }
}

extension ConnectionState {
    var label: String {
        switch self {
        case .connected: "Connected"
        case .connecting: "Connecting…"
        case .disconnected(let error): Self.explain(error)
        }
    }

    /// The state, with how many addresses a connection under way tries: "Connecting… (trying
    /// 3 addresses)".
    func labelCounting(addresses: Int) -> String {
        self == .connecting && addresses > 1 ? "Connecting… (trying \(addresses) addresses)" : label
    }

    /// What a message sent to a machine in this state waits for, as the machine takes it only
    /// once connected; `nil` while connected, when it is on its way.
    func holdUp(addresses: Int) -> String? {
        switch self {
        case .connected: nil
        case .connecting:
            addresses > 1 ? "Connecting to the machine (trying \(addresses) addresses)…" : "Connecting to the machine…"
        case .disconnected(let error): "The machine is offline (\(Self.explain(error))). Sends once it connects."
        }
    }

    /// A disconnection as the user can act on it: a daemon on another protocol version needs
    /// updating, which the raw error buries among unreachable addresses.
    static func explain(_ error: String) -> String {
        guard let theirsAt = error.range(of: "this daemon speaks "),
              let oursAt = error.range(of: "protocol version ") else { return error }
        let theirs = error[theirsAt.upperBound...].prefix { $0.isNumber }
        let ours = error[oursAt.upperBound...].prefix { $0.isNumber }
        return "Runs a different herder (protocol \(theirs); this app speaks \(ours)). Update one of them to connect."
    }
}
