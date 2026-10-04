import Herder
@testable import HerderKit
import Testing

struct TurnLimitTests {
    @Test func turnsAreWordedForTheCardSettingsAndAWaitingSession() {
        let full = TurnLoad(running: 2, max: 2, waiting: 1, constraint: .maxTurns)
        #expect(full.fraction == "2/2 turns")
        #expect(full.usage == "2 running · 1 waiting")
        #expect(full.waitingHint == "Waiting for a free slot · 2 of 2 turns in use")
        #expect(TurnLoad(running: 1, max: 1, waiting: 1).waitingHint
            == "Waiting for a free slot · 1 of 1 turn in use")
        let short = TurnLoad(running: 1, max: 4, waiting: 1, constraint: .memory)
        #expect(short.waitingHint == "Waiting for free memory on this machine")
        #expect(TurnLoad.ceiling == 64)
    }

    @MainActor
    @Test func picksAreSentOneAtATimeAndCollapseIntoTheLatest() async {
        var sent: [Int] = []
        var release: CheckedContinuation<Void, Never>?
        let setter = TurnLimitSetter { value in
            sent.append(value)
            if sent.count == 1 { await withCheckedContinuation { release = $0 } }
        }
        #expect(setter.shown(2) == 2)
        setter.pick(3)
        #expect(await eventually { release != nil })
        setter.pick(4)
        setter.pick(5)
        #expect(setter.shown(2) == 5)
        release?.resume()
        #expect(await eventually { sent == [3, 5] })
        setter.reported(3)
        #expect(setter.shown(3) == 5, "an older limit arriving keeps the latest pick")
        setter.reported(5)
        #expect(setter.shown(5) == 5)
        #expect(setter.error == nil)
    }

    @MainActor
    @Test func aRefusedPickShowsItsErrorAndTheMachinesLimitAgain() async {
        struct Refused: Error {}
        let setter = TurnLimitSetter { _ in throw Refused() }
        setter.pick(9)
        #expect(await eventually { setter.error != nil })
        #expect(setter.shown(2) == 2)
    }

    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func anOwnerChangesTheTurnLimitLive() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let machine = try await fleet.pair(daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        #expect(await eventually { fleet.lists.machines.first?.turns?.max == 4 })

        let setter = TurnLimitSetter(fleet: fleet, hostId: machine.hostId)
        setter.pick(6)
        #expect(await eventually { fleet.lists.machines.first?.turns?.max == 6 })
        #expect(fleet.lists.machines.first?.turns?.fraction == "0/6 turns")

        do {
            _ = try await fleet.client.send(hostId: machine.hostId, command: .setResourceLimits(maxTurns: 0))
            Issue.record("a limit of 0 must be refused")
        } catch let error as HerderError {
            guard case .Rejected(let info) = error else { throw error }
            #expect(info.code == .badRequest)
        }
    }
}
