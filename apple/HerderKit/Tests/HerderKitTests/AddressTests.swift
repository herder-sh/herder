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

    /// A connection under way says how many addresses it tries, and a message says what it
    /// waits for instead of "Sending…" while its machine is not connected.
    @Test func aConnectionSaysWhatItTriesAndWhatAMessageWaitsFor() {
        #expect(ConnectionState.connecting.labelCounting(addresses: 3) == "Connecting… (trying 3 addresses)")
        #expect(ConnectionState.connecting.labelCounting(addresses: 1) == "Connecting…")
        #expect(ConnectionState.connected.labelCounting(addresses: 3) == "Connected")
        #expect(ConnectionState.disconnected(error: "refused").labelCounting(addresses: 3) == "refused")
        #expect(ConnectionState.connected.holdUp(addresses: 3) == nil)
        #expect(ConnectionState.connecting.holdUp(addresses: 3) == "Connecting to the machine (trying 3 addresses)…")
        #expect(ConnectionState.connecting.holdUp(addresses: 1) == "Connecting to the machine…")
        #expect(ConnectionState.disconnected(error: "refused").holdUp(addresses: 1)
            == "The machine is offline (refused). Sends once it connects.")
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
