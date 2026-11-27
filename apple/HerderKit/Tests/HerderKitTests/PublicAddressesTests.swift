import Herder
@testable import HerderKit
import Testing

struct PublicAddressesTests {
    /// An owner adds a public address from the app: the machine saves it as every device
    /// dials it, and this device has it among the machine's addresses from its next
    /// connection, with nothing typed there. Removed, it leaves them again.
    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func anOwnerSetsThePublicAddressesEveryDevicePicksUp() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let machine = try await fleet.signIn(to: daemon)
        let hostId = machine.hostId
        try await fleet.client.synced(hostId: hostId)
        #expect(try await fleet.publicAddresses(on: hostId).isEmpty)

        let saved = try await fleet.setPublicAddresses(["https://box.example.com/", "203.0.113.7"], on: hostId)
        #expect(saved == ["wss://box.example.com", "203.0.113.7:7447"])
        #expect(try await fleet.publicAddresses(on: hostId) == saved)
        _ = try await fleet.client.reconnect(hostId: hostId)
        func addresses() -> [String] { fleet.client.machines().first { $0.hostId == hostId }?.addresses ?? [] }
        #expect(await eventually(within: 30) { saved.allSatisfy { addresses().contains($0) } })

        do {
            _ = try await fleet.setPublicAddresses(["127.0.0.1:7447"], on: hostId)
            Issue.record("a loopback public address must be refused")
        } catch let error as HerderError {
            guard case .Rejected(let info) = error else { throw error }
            #expect(info.code == .badRequest)
        }

        #expect(try await fleet.setPublicAddresses([], on: hostId).isEmpty)
        _ = try await fleet.client.reconnect(hostId: hostId)
        #expect(await eventually(within: 30) { !addresses().contains { saved.contains($0) } })
    }
}
