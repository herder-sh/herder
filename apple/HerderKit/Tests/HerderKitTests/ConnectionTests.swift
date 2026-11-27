import Foundation
import Herder
@testable import HerderKit
import Testing

/// What the apps say about how a machine is reached (§5.9): nothing while all is well, a word
/// when it is not, and the whole of it on the connection card.
@MainActor struct ConnectionTests {
    /// A connected machine at `address`, with `average` ms round trips when given.
    static func connected(at address: String, average: UInt32? = nil) -> Machine {
        var devbox = machine("devbox", name: "devbox", sessions: [])
        devbox.addresses = [address]
        devbox.address = address
        devbox.quality.lastRttMs = average
        devbox.quality.averageRttMs = average
        devbox.quality.minRttMs = average
        devbox.quality.maxRttMs = average
        return devbox
    }

    @Test func theRouteNamesWhatTheConnectionGoesOverAndItsTransport() {
        var hub = machine("v1", name: "herder-hub", sessions: [])
        hub.hosts = [FleetHost(hostId: "devbox", hostName: "devbox", online: true, lastSeen: "")]
        func route(_ address: String) -> String { MachineRoute(address: address, among: [hub]).label }
        #expect(route("192.168.1.5:7447") == "Local network · WebSocket")
        #expect(route("devbox.local:7447") == "Local network · WebSocket")
        #expect(route("100.101.102.103:7447") == "Tailscale · WebSocket")
        #expect(route("devbox.tail1234.ts.net:7447") == "Tailscale · WebSocket")
        #expect(route("wss://devbox.example.com") == "Tunnel · WebSocket")
        #expect(route("devbox.example.com:7447") == "Internet · WebSocket")
        #expect(route("hub://v1") == "Through herder-hub · WebSocket")
        // Not connected, there is no route.
        var offline = Self.connected(at: "192.168.1.5:7447")
        offline.connection = .disconnected(error: "no answer")
        #expect(MachineRoute(offline, among: [offline]) == nil)
    }

    @Test func onlyASignalThatIsNotGoodIsSaid() {
        #expect(ConnectionSignal(Self.connected(at: "a:7447", average: 40)) == .good)
        #expect(ConnectionSignal(Self.connected(at: "a:7447")) == .good)
        #expect(ConnectionSignal(Self.connected(at: "a:7447", average: 640)) == .slow)
        #expect(ConnectionSignal(machine("m", name: "m", sessions: [], connection: .connecting)) == .connecting)
        #expect(ConnectionSignal(machine("m", name: "m", sessions: [], connection: .disconnected(error: "x"))) == .offline)
        #expect(ConnectionSignal.good.word == nil)
        #expect([ConnectionSignal.slow, .connecting, .offline].map(\.word) == ["Slow", "Connecting…", "Offline"])
    }

    @Test func theCardSaysRouteRoundTripUptimeAndReconnects() {
        let now = Date(timeIntervalSince1970: 1_000_000)
        var devbox = Self.connected(at: "devbox.tail1234.ts.net:7447", average: 41)
        devbox.quality.lastRttMs = 38
        devbox.quality.reconnects = 2
        let log = [ConnectionChange(at: now.addingTimeInterval(-720), state: .connected)]
        let facts = ConnectionFacts(machine: devbox, among: [devbox], log: log, now: now)
        #expect(facts.state == "Connected")
        #expect(facts.signal == .good)
        #expect(facts.rows.map(\.label) == ["Route", "Address", "Round trip", "Connected for", "Reconnects"])
        #expect(facts.rows.map(\.value) == [
            "Tailscale · WebSocket", "devbox.tail1234.ts.net:7447", "38 ms · 41 ms on average",
            MachinePage.duration(720), "2 since the app opened",
        ])
    }

    @Test func aSlowLinkSaysWhy() {
        let devbox = Self.connected(at: "hub://v1", average: 640)
        let facts = ConnectionFacts(machine: devbox, among: [devbox], log: [], now: .now)
        #expect(facts.signal == .slow)
        // A relayed route has no address of its own to show.
        #expect(!facts.rows.map(\.label).contains("Address"))
        #expect(facts.rows.first { $0.label == "Link" }?.value.hasPrefix("Slow: 640 ms on average") == true)
    }

    @Test func anOfflineCardSaysWhenItWasLastConnectedAndWhy() {
        let now = Date(timeIntervalSince1970: 1_000_000)
        let devbox = machine("devbox", name: "devbox", sessions: [], connection: .disconnected(error: "no answer in 10s"))
        let log = [
            ConnectionChange(at: now.addingTimeInterval(-900), state: .connected),
            ConnectionChange(at: now.addingTimeInterval(-180), state: .disconnected(error: "no answer in 10s")),
        ]
        let facts = ConnectionFacts(machine: devbox, among: [devbox], log: log, now: now)
        #expect(facts.signal == .offline)
        #expect(facts.rows.map(\.label) == ["Last connected", "Why"])
        #expect(facts.rows.map(\.value) == ["\(MachinePage.duration(180)) ago", "no answer in 10s"])
    }

    @Test func theMessagesRowCountsWhereEachMessageIs() {
        #expect(ConnectionFacts.messages([]) == nil)
        let outbox = [
            Outgoing(text: "a", state: .sending), Outgoing(text: "b", state: .delivered),
            Outgoing(text: "c", state: .delivered), Outgoing(text: "d", state: .failed("refused")),
        ]
        #expect(ConnectionFacts.messages(outbox) == "1 sending · 2 delivered, not started · 1 not sent")
    }

    /// A message to a connected machine says nothing in its first second, as almost every one
    /// is delivered by then.
    @Test func aMessageIsQuietForItsFirstSecond() {
        #expect(Outgoing.quiet < Outgoing.overdue)
        let sent = Date(timeIntervalSince1970: 1_000_000)
        let devbox = Self.connected(at: "a:7447")
        #expect(Outgoing(text: "x", state: .sending, sentAt: sent).waiting(at: sent.addingTimeInterval(0.5), for: devbox) == nil)
    }
}
