import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct AddressTests {
    @Test func anAddressShowsWhetherItIsInUseConnectingOrFailed() {
        let failure = AddressStatus.Failure(address: "b:7447", message: "no answer")
        func status(_ address: String, connecting: String? = nil) -> AddressStatus {
            AddressStatus(address: address, inUse: "a:7447", connecting: connecting, failure: failure)
        }
        #expect(status("a:7447") == .inUse)
        #expect(status("b:7447") == .failed("no answer"))
        #expect(status("c:7447") == .available)
        // A Connect under way shows over the last one's failure, and over "In use".
        #expect(status("b:7447", connecting: "b:7447") == .connecting)
        #expect(status("a:7447", connecting: "a:7447") == .connecting)
        // Once in use, an earlier failure no longer shows.
        #expect(AddressStatus(address: "b:7447", inUse: "b:7447", connecting: nil, failure: failure) == .inUse)
    }

    /// The route through a hub reads as the hub's name wherever an address shows, never
    /// as its host id.
    @Test func aMachineReachedThroughItsHubNamesTheHub() throws {
        var hub = machine("v1", name: "home hub", sessions: [])
        hub.hosts = [FleetHost(hostId: "h1", hostName: "box", online: true, lastSeen: "")]
        var box = machine("h1", name: "box", sessions: [])
        box.addresses = ["192.168.1.5:7447", "hub://v1"]
        box.address = "hub://v1"
        let machines = [hub, box]
        #expect(MachineRow.via("hub://v1", among: machines) == "home hub")
        #expect(MachineRow.label("hub://v1", among: machines) == "through home hub")
        #expect(MachineRow.label("192.168.1.5:7447", among: machines) == "192.168.1.5:7447")
        // A machine that is only named hub is an address like any other.
        #expect(MachineRow.via("hub.example.com:7447", among: machines) == nil)
        #expect(MachineRow.label("wss://hub.example.com", among: machines) == "wss://hub.example.com")
        // A route through a hub this device has left still reads as one.
        #expect(MachineRow.label("hub://gone", among: machines) == "through a hub")

        let row = try #require(MachineRow.rows(machines, summaries: []).first { $0.hostId == "h1" })
        #expect(row.reach == .through("home hub"))
        #expect(row.reach.label == "Through home hub")
        box.address = "192.168.1.5:7447"
        let direct = try #require(MachineRow.rows([hub, box], summaries: []).first { $0.hostId == "h1" })
        #expect(direct.reach == .direct("Local network"))
        // The route is not this Mac, whatever this Mac is called.
        #expect(!MachineRow.isLocal("hub://v1", names: ["hub"]))
    }

    /// A connection under way says how many addresses it tries, and a message says what it
    /// waits for instead of "Sending…" while its machine is not connected.
    @Test func aConnectionSaysWhatItTriesAndWhatAMessageWaitsFor() {
        #expect(ConnectionState.connecting.labelCounting(addresses: 3) == "Connecting… (trying 3 addresses)")
        #expect(ConnectionState.connecting.labelCounting(addresses: 1) == "Connecting…")
        #expect(ConnectionState.connected.labelCounting(addresses: 3) == "Connected")
        #expect(ConnectionState.disconnected(error: "refused").labelCounting(addresses: 3) == "refused")
        #expect(ConnectionState.connected.holdUp(machine: "devbox", addresses: 3) == nil)
        #expect(ConnectionState.connecting.holdUp(machine: "devbox", addresses: 3)
            == "Waiting for devbox: connecting (trying 3 addresses)…")
        #expect(ConnectionState.connecting.holdUp(machine: "devbox", addresses: 1) == "Waiting for devbox: connecting…")
        // Why it is offline is on the connection card, not under every message.
        #expect(ConnectionState.disconnected(error: "refused").holdUp(machine: "devbox", addresses: 1)
            == "Waiting for devbox: offline · sends when it is back")
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func connectingThroughAnAddressPutsItFirstAndSaysWhenItDidNotAnswer() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let host = try await fleet.signIn(to: daemon).hostId
        try await fleet.client.synced(hostId: host)
        let live = try #require(fleet.machines.first?.addresses.first)
        // Nothing listens on port 1.
        let dead = "127.0.0.1:1"
        try fleet.setAddresses(host, to: [live, dead])

        await #expect(throws: HerderError.self) { try await fleet.connect(host, through: dead) }
        #expect(fleet.machines.first?.addresses == [dead, live])
        #expect(fleet.machines.first?.address == live)

        try await fleet.connect(host, through: live)
        #expect(fleet.machines.first?.addresses == [live, dead])
        #expect(fleet.machines.first?.address == live)
        #expect(fleet.machines.first?.connection == .connected)
    }
}
