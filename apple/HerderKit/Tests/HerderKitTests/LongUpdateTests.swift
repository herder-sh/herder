import Foundation
import Herder
@testable import HerderKit
import Testing

/// A long update, as a session's cached journal on a launch or what a machine sends after days
/// away, folds away from the main actor: the app draws and takes input meanwhile.
@MainActor struct LongUpdateTests {
    /// What ran on the main actor, in order.
    private final class Steps {
        var taken: [String] = []
    }

    /// A fleet following `01A` on `host-a`, and the script of its events.
    private func fleet() throws -> (Fleet, Script) {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        var script = Script("01A")
        fleet.setMachinesForTesting([machine("host-a", name: "a", sessions: ["01A"])])
        fleet.applyForTesting(SessionUpdate(events: [script.event(created())], streaming: []), to: script.key)
        fleet.isScrolling = { false }
        return (fleet, script)
    }

    @Test func theMainActorRunsOtherWorkWhileALongUpdateFolds() async throws {
        var (fleet, script) = try fleet()
        let journal = longSession(&script).dropFirst()
        let steps = Steps()
        // Waiting for the main actor before the update arrives, as a frame to draw would.
        let other = Task { steps.taken.append("other work") }
        #expect(await fleet.receive(SessionUpdate(events: Array(journal), streaming: []), for: script.key))
        steps.taken.append("folded")
        await other.value
        #expect(steps.taken == ["other work", "folded"])
        #expect(fleet.session(script.key)?.timeline.count == journal.count + 1)
        #expect(fleet.session(script.key)?.stats.turns == 100)
    }

    /// A turn streams an event or a few at a time: folded where they arrive, with no trip away.
    @Test func aShortUpdateFoldsAtOnce() async throws {
        var (fleet, script) = try fleet()
        let steps = Steps()
        let other = Task { steps.taken.append("other work") }
        let renamed = SessionUpdate(events: [script.event(.titleChanged(title: "Renamed", source: .user))], streaming: [])
        #expect(await fleet.receive(renamed, for: script.key))
        steps.taken.append("folded")
        await other.value
        #expect(steps.taken == ["folded", "other work"])
        #expect(fleet.session(script.key)?.title == "Renamed")
    }

    /// What the user does to a session while its long update folds is kept: the update folds
    /// over it.
    @Test func aChangeMadeHereWhileALongUpdateFoldsIsKept() async throws {
        var (fleet, script) = try fleet()
        let failed: [EventBody] = [.turnStarted(turnId: "f"), .turnFailed(turnId: "f", error: TurnError(class: .fatal, message: "boom"))]
        fleet.applyForTesting(SessionUpdate(events: failed.map { script.event($0) }, streaming: []), to: script.key)
        try #require(fleet.session(script.key)?.standingFailure != nil)
        let journal = longSession(&script).dropFirst()
        // The user closes the failure's card while the update folds.
        let closing = Task { fleet.dismissFailure(script.key) }
        #expect(await fleet.receive(SessionUpdate(events: Array(journal), streaming: []), for: script.key))
        await closing.value
        let failure = fleet.session(script.key)?.log.compactMap { entry -> Failure? in
            if case .failure(let failure) = entry { failure } else { nil }
        }.first
        #expect(failure?.resolution == .dismissed)
        #expect(fleet.session(script.key)?.stats.turns == 101)
    }
}
