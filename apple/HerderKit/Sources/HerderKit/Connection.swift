import Foundation
import Herder
import SwiftUI

/// What the connection in use to a machine goes over: the kind of route its address names, and
/// the transport. Said as "Tailscale · WebSocket".
struct MachineRoute: Hashable {
    enum Kind: Hashable {
        /// A private range, loopback, or a name only a local network resolves.
        case local
        /// Tailscale's ranges and `*.ts.net`.
        case tailscale
        /// A `wss://` tunnel URL.
        case tunnel
        /// Any other public name or address.
        case internet
        /// Relayed by the hub of that name.
        case hub(String)
        /// herder's own WireGuard client, to the user's WireGuard server (a `wg://` route).
        case ownVpn
        /// herder's own WireGuard client, to the hub's WireGuard endpoint.
        case hubWireGuard
        /// The Durable Object relay in the user's Cloudflare account (`relay+wss://`).
        case relay
    }

    let kind: Kind
    /// The address in use, as `Machine::address` gives it.
    let address: String
    /// Every connection is a WebSocket until QUIC (P26.9) gives the client core a second
    /// transport to name.
    let transport = "WebSocket"

    init(address: String, among machines: [Machine]) {
        self.address = address
        switch routeKind(address: address) {
        case .ownVpn:
            kind = .ownVpn
            return
        case .hubWireGuard:
            kind = .hubWireGuard
            return
        case .cloudflareRelay:
            kind = .relay
            return
        default:
            break
        }
        if let hub = MachineRow.via(address, among: machines) {
            kind = .hub(hub)
        } else if address.hasPrefix("wss://") {
            kind = .tunnel
        } else {
            kind = switch AddressScope(address) {
            case .network: .local
            case .vpn: .tailscale
            case .anywhere: .internet
            }
        }
    }

    /// The route of `machine`'s connection; `nil` while it is not connected.
    init?(_ machine: Machine, among machines: [Machine]) {
        guard machine.connection == .connected, let address = machine.address else { return nil }
        self.init(address: address, among: machines)
    }

    /// "Tailscale", "Through herder-hub".
    var name: String {
        switch kind {
        case .local: "Local network"
        case .tailscale: "Tailscale"
        case .tunnel: "Tunnel"
        case .internet: "Internet"
        case .hub(let hub): "Through \(hub)"
        case .ownVpn: "Your VPN"
        case .hubWireGuard: "Hub WireGuard"
        case .relay: "Cloudflare relay"
        }
    }

    /// "Tailscale · WebSocket".
    var label: String { "\(name) · \(transport)" }
}

/// How a machine's connection stands, as the chrome says it: nothing while it is good, a mark
/// and a word otherwise.
enum ConnectionSignal: Hashable {
    case good
    /// Connected, with round trips of `slowMs` or more on average on this connection.
    case slow
    case connecting
    case offline

    /// The average round trip from which a link reads slow: sending and streaming lag by then.
    static let slowMs: UInt32 = 300

    init(_ machine: Machine) {
        switch machine.connection {
        case .connected: self = (machine.quality.averageRttMs ?? 0) >= Self.slowMs ? .slow : .good
        case .connecting: self = .connecting
        case .disconnected: self = .offline
        }
    }

    /// "Offline"; `nil` while good, when nothing is said.
    var word: String? {
        switch self {
        case .good: nil
        case .slow: "Slow"
        case .connecting: "Connecting…"
        case .offline: "Offline"
        }
    }
}

/// A signal that is not good as its mark and word: a dashed ring connecting, a ring offline
/// (never red: a laptop asleep is expected), an amber tortoise when slow. Nothing while good.
struct SignalLabel: View {
    let signal: ConnectionSignal

    var body: some View {
        if let word = signal.word {
            HStack(spacing: 4) {
                switch signal {
                case .slow:
                    Image(systemName: "tortoise.fill").font(.caption2).foregroundStyle(Theme.accent)
                case .connecting:
                    ReachMark(reach: .connecting)
                default:
                    ReachMark(reach: .offline)
                }
                Text(word).foregroundStyle(signal == .slow ? Theme.accent : Theme.secondary)
            }
            .fixedSize()
            .accessibilityElement(children: .combine)
        }
    }
}

/// What the connection card says about a machine, row by row; a row is left out when it has
/// nothing to say.
struct ConnectionFacts: Equatable {
    struct Row: Equatable, Identifiable {
        let label: String
        let value: String
        var mono = false
        var id: String { label }
    }

    let name: String
    let state: String
    let signal: ConnectionSignal
    let rows: [Row]

    /// `log` is the machine's connection log since the app opened, oldest first.
    init(machine: Machine, among machines: [Machine], log: [ConnectionChange], now: Date) {
        name = machine.name
        state = machine.connection.labelCounting(addresses: machine.addresses.count)
        signal = ConnectionSignal(machine)
        var rows: [Row] = []
        let route = MachineRoute(machine, among: machines)
        if let route {
            rows.append(Row(label: "Route", value: route.label))
            if case .hub = route.kind {} else { rows.append(Row(label: "Address", value: route.address, mono: true)) }
        }
        let quality = machine.quality
        if machine.connection == .connected, let last = quality.lastRttMs {
            let average = quality.averageRttMs.map { " · \($0) ms on average" } ?? ""
            rows.append(Row(label: "Round trip", value: "\(last) ms\(average)"))
            if let verdict = LinkVerdict(quality), verdict.level != .good {
                rows.append(Row(label: "Link", value: [verdict.summary, verdict.advice].filter { !$0.isEmpty }
                    .joined(separator: ". ")))
            }
        }
        let health = ConnectionHealth(log: log, now: now)
        if let up = health.currentUp {
            rows.append(Row(label: "Connected for", value: MachinePage.duration(up)))
        } else if let last = log.last(where: { $0.state == .connected }),
                  let end = log.first(where: { $0.at > last.at }) {
            let seconds = now.timeIntervalSince(end.at)
            rows.append(Row(label: "Last connected", value: seconds < 60 ? "just now" : "\(MachinePage.duration(seconds)) ago"))
        }
        if quality.reconnects > 0 {
            rows.append(Row(label: "Reconnects", value: "\(quality.reconnects) since the app opened"))
        }
        if case .disconnected(let error) = machine.connection, !error.isEmpty {
            rows.append(Row(label: "Why", value: ConnectionState.explain(error)))
        }
        self.rows = rows
    }

    /// "1 sending · 2 delivered, not started · 1 not sent": where this device's messages to the
    /// machine are; `nil` when none is on its way.
    static func messages(_ outbox: [Outgoing]) -> String? {
        var sending = 0, delivered = 0, failed = 0
        for outgoing in outbox {
            switch outgoing.state {
            case .sending: sending += 1
            case .delivered: delivered += 1
            case .failed: failed += 1
            }
        }
        let parts = [
            sending > 0 ? "\(sending) sending" : nil,
            delivered > 0 ? "\(delivered) delivered, not started" : nil,
            failed > 0 ? "\(failed) not sent" : nil,
        ]
        let said = parts.compactMap(\.self)
        return said.isEmpty ? nil : said.joined(separator: " · ")
    }
}

/// How this device reaches a machine, in full: its state, route and transport, round trip, how
/// long it has been up, reconnects, why it is offline, and this device's messages on their way
/// to it. Opened from a session's status line and a message's delivery line; the head of a
/// machine page's Connection section.
struct ConnectionCard: View {
    let fleet: Fleet
    let hostId: HostId
    /// Shows the machine's name and state on top; a machine page has them in its header.
    var titled = true
    /// Opens the machine's page, from the card's foot; `nil` on the page itself.
    var openMachine: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let machine = fleet.machines.first(where: { $0.hostId == hostId }) {
                let facts = ConnectionFacts(machine: machine, among: fleet.machines,
                                            log: fleet.connectionLog[hostId] ?? [], now: .now)
                if titled {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        ConnectionMark(state: machine.connection)
                        Text(facts.name).font(.headline).foregroundStyle(Theme.text)
                        Text(facts.state).font(.subheadline).foregroundStyle(Theme.secondary).lineLimit(2)
                    }
                    .padding(.bottom, 2)
                }
                ForEach(facts.rows) { row in
                    DetailRow(label: row.label, value: row.value, mono: row.mono)
                }
                OutboxRow(fleet: fleet, hostId: hostId)
                if let openMachine {
                    Button("Machine Page", action: openMachine)
                        .buttonStyle(.plain)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Theme.link)
                        .padding(.top, 4)
                        .accessibilityIdentifier("connection-machine-page")
                }
            } else {
                Text("This machine is no longer paired with this device.").foregroundStyle(Theme.secondary)
            }
        }
        .padding(titled ? 16 : 0)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityIdentifier("connection-card")
    }
}

/// The card's Messages row: a view of its own, as it reads every session's outbox, so only it
/// draws again as any session changes.
private struct OutboxRow: View {
    let fleet: Fleet
    let hostId: HostId

    var body: some View {
        let outbox = fleet.sessions.filter { $0.key.hostId == hostId }.values.flatMap(\.outbox)
        if let messages = ConnectionFacts.messages(outbox) {
            DetailRow(label: "Messages", value: messages)
        }
    }
}

extension View {
    /// Opens `hostId`'s connection card while `isPresented`: a popover on the Mac and iPad, a
    /// sheet on a phone. Machine Page closes it and opens the machine's page.
    func connectionCard(isPresented: Binding<Bool>, fleet: Fleet, hostId: HostId, pages: PageNavigator) -> some View {
        popover(isPresented: isPresented) {
            ConnectionCard(fleet: fleet, hostId: hostId) {
                isPresented.wrappedValue = false
                pages.open(.machine(hostId))
            }
            #if os(macOS)
            .frame(width: 380)
            #else
            .frame(minWidth: 320, maxWidth: 420)
            .presentationDetents([.medium, .large])
            .presentationBackground(Theme.sheet)
            #endif
        }
    }
}
