import Foundation
import Herder
@testable import HerderKit
import Observation
import Synchronization
import Testing

/// What a view that reads the machines redraws on: a running turn moves its session's head
/// several times a second, and a page of accounts or settings must not redraw for it.
@MainActor struct FleetObservationTests {
    private func fleet() throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        fleet.setMachinesForTesting([machine("devbox", name: "devbox", sessions: ["01A"])])
        return fleet
    }

    /// Whether reading `read` would have redrawn a view after `change`.
    private func redraws(_ read: @escaping () -> Void, after change: () -> Void) -> Bool {
        let changed = Mutex(false)
        withObservationTracking(read) { changed.withLock { $0 = true } }
        change()
        return changed.withLock { $0 }
    }

    @Test func aSessionMovingOnRedrawsOnlyWhatShowsSessions() throws {
        let fleet = try fleet()
        var moved = fleet.machines
        moved[0].sessions[0].status = .running
        #expect(!redraws({ _ = fleet.machines }) { fleet.setMachinesForTesting(moved) })
        moved[0].sessions[0].status = .idle
        #expect(redraws({ _ = fleet.liveMachines }) { fleet.setMachinesForTesting(moved) })
        // Either way, both say how the machines are now.
        #expect(fleet.machines[0].sessions[0].status == .idle)
    }

    @Test func aMachineChangingRedrawsBoth() throws {
        let fleet = try fleet()
        var renamed = fleet.machines
        renamed[0].name = "devbox-2"
        #expect(redraws({ _ = fleet.machines }) { fleet.setMachinesForTesting(renamed) })
        renamed[0].name = "devbox-3"
        #expect(redraws({ _ = fleet.liveMachines }) { fleet.setMachinesForTesting(renamed) })
    }

    @Test func theSameMachinesAgainRedrawNothing() throws {
        let fleet = try fleet()
        let same = fleet.machines
        #expect(!redraws({ _ = fleet.liveMachines }) { fleet.setMachinesForTesting(same) })
    }
}
