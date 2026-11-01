import Foundation
import Herder
@testable import HerderKit
import Testing

/// The fleet's changes wait while a scroll is in progress, for `Fleet.scrollHold` at most, and
/// what needs the user does not wait.
@MainActor
@Suite(.serialized)
struct ScrollDeferralTests {
    #if os(iOS)
    private final class Seen: @unchecked Sendable { var value = false }

    /// On iOS a scroll is in progress while the main run loop tracks one.
    @Test func aScrollIsInProgressWhileTheRunLoopTracks() {
        let during = Seen()
        RunLoop.main.perform(inModes: [Fleet.scrollMode]) {
            MainActor.assumeIsolated { during.value = Fleet.scrolling }
        }
        RunLoop.main.run(mode: Fleet.scrollMode, before: .now + 0.3)
        #expect(during.value)
        #expect(!Fleet.scrolling)
    }
    #else
    /// A fleet following `01A` on `host-a`, and the script of its events.
    private func fleet() throws -> (Fleet, Script) {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        var script = Script("01A")
        fleet.setMachinesForTesting([machine("host-a", name: "a", sessions: ["01A"])])
        fleet.applyForTesting(SessionUpdate(events: [script.event(created())], streaming: []), to: script.key)
        return (fleet, script)
    }

    /// A scroll that never ends, as a wheel that keeps turning, for `fleet` alone.
    private func endlessScroll(_ fleet: Fleet) { fleet.isScrolling = { true } }

    @Test func nothingWaitsWithoutAScroll() async throws {
        let (fleet, _) = try fleet()
        fleet.isScrolling = { false }
        let started = ContinuousClock.now
        await fleet.scrollEnded()
        #expect(ContinuousClock.now - started < .milliseconds(50))
    }

    /// That it applies at all shows the hold ends: an endless wait runs into the time limit. No
    /// bound on the wait itself, which on a busy CI runner measures how long the main actor
    /// waited for its turn (8-12 s seen), not the hold.
    @Test(.timeLimit(.minutes(1)))
    func aChangeDuringAnEndlessScrollAppliesWithinTheHold() async throws {
        var (fleet, script) = try fleet()
        endlessScroll(fleet)
        let renamed = SessionUpdate(events: [script.event(.titleChanged(title: "Renamed", source: .user))], streaming: [])
        let started = ContinuousClock.now
        #expect(await fleet.receive(renamed, for: script.key))
        #expect(ContinuousClock.now - started >= .milliseconds(450))
        #expect(fleet.session(script.key)?.title == "Renamed")
    }

    @Test func anApprovalDuringAScrollAppliesAtOnce() async throws {
        var (fleet, script) = try fleet()
        endlessScroll(fleet)
        let asked = SessionUpdate(events: [script.event(.approvalRequested(
            approvalId: "a", turnId: "t", toolCallId: "i", summary: "rm -rf build", routedTo: .user, reason: nil,
            secret: nil))], streaming: [])
        let started = ContinuousClock.now
        #expect(await fleet.receive(asked, for: script.key))
        #expect(ContinuousClock.now - started < .milliseconds(100))
        #expect(fleet.lists.requests.map(\.requestId) == ["a"])
    }
    #endif

    @Test func whatNeedsTheUserIsUrgent() {
        var script = Script("01A")
        let question = script.event(.questionAsked(questionId: "q", turnId: "t", text: "Which?", choices: [],
                                                   multiSelect: false, routedTo: .user, reason: nil))
        let renamed = script.event(.titleChanged(title: "Renamed", source: .user))
        #expect(SessionUpdate(events: [renamed, question], streaming: []).needsUser)
        #expect(!SessionUpdate(events: [renamed], streaming: []).needsUser)
    }
}
