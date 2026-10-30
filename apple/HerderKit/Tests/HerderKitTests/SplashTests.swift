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

    @Test func theFleetHasOpenedOnceOneMachineConnectsWhileAnotherIsStillConnecting() throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("the profile did not open")
            return
        }
        var asleep = machine("host-b", name: "b", sessions: [])
        asleep.connection = .connecting
        fleet.setMachinesForTesting([asleep])
        #expect(!fleet.opened)
        fleet.setMachinesForTesting([machine("host-a", name: "a", sessions: []), asleep])
        #expect(fleet.opened)
    }

    /// Cached sessions show at once: an asleep machine does not hold the splash over them.
    @Test func theCachedBoardIsReadyBeforeAnyMachineConnects() throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("the profile did not open")
            return
        }
        fleet.setMachinesForTesting([machine("host-a", name: "a", sessions: [], connection: .connecting)])
        #expect(!fleet.ready)
        fleet.setMachinesForTesting([machine("host-a", name: "a", sessions: ["01A"], connection: .connecting)])
        #expect(!fleet.opened)
        #expect(fleet.ready)
    }

    /// On a launch, the Board lists what the offline cache holds while its machine is down.
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func theOfflineCacheShowsBeforeAnyMachineConnects() async throws {
        let profile = temporaryProfile()
        do {
            let daemon = try FakeDaemon()
            guard case .opened(let fleet) = Profile.open(at: profile, client: "test") else {
                Issue.record("the profile did not open")
                return
            }
            let following = Task { await fleet.follow() }
            defer { following.cancel() }
            let machine = try await fleet.signIn(to: daemon)
            try await fleet.client.synced(hostId: machine.hostId)
            _ = try await fleet.createSession(
                on: machine.hostId, repo: daemon.repo, projectId: nil, accountId: daemon.account, model: "",
                mode: .ask, prompt: "Say hello.")
            #expect(await eventually { !fleet.lists.board().isEmpty })
            // Saves the offline cache.
            fleet.suspend()
        }
        // Relaunched with the daemon gone: nothing ever connects.
        guard case .opened(let fleet) = Profile.open(at: profile, client: "test") else {
            Issue.record("the profile did not reopen")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        #expect(await eventually(within: 2) { !fleet.lists.board().isEmpty })
        #expect(fleet.ready)
        #expect(!fleet.machines.contains { $0.connection == .connected })
    }
}
