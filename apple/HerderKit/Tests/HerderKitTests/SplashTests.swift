import Herder
@testable import HerderKit
import Testing

@MainActor
struct SplashTests {
    @Test func theFleetHasOpenedOnceNoMachineIsStillConnecting() throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("the profile did not open")
            return
        }
        #expect(fleet.opened)
        var connecting = machine("host-a", name: "a", sessions: [])
        connecting.connection = .connecting
        var failed = machine("host-b", name: "b", sessions: [])
        failed.connection = .disconnected(error: "refused")
        fleet.setMachinesForTesting([connecting, failed])
        #expect(!fleet.opened)
        fleet.setMachinesForTesting([machine("host-a", name: "a", sessions: []), failed])
        #expect(fleet.opened)
    }
}
