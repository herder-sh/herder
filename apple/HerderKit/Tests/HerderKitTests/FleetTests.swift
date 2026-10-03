import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct FleetTests {
    @Test func aProfileThatCannotBeOpenedSaysWhy() throws {
        let file = temporaryProfile()
        try Data().write(to: file)
        guard case .failed(let message) = Profile.open(at: file, client: "test") else {
            Issue.record("opened a profile at a file")
            return
        }
        #expect(!message.isEmpty)
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func pairingShowsTheMachineConnected() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        #expect(fleet.machines.isEmpty)
        let following = Task { await fleet.follow() }
        defer { following.cancel() }

        let machine = try await fleet.pair(link: daemon.link)
        #expect(machine.name == "fake-host")
        #expect(fleet.machines.map(\.hostId) == [machine.hostId])
        #expect(await eventually { fleet.machines.first?.connection == .connected })
        #expect(await eventually { fleet.machines.first?.role == .owner })
    }
}
