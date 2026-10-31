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

    /// A scroll that never ends, as a wheel that keeps turning, until `stop`.
    private func endlessScroll() { Fleet.lastScroll = .distantFuture }

    private func stop() { Fleet.lastScroll = .distantPast }

    @Test func nothingWaitsWithoutAScroll() async {
        Fleet.lastScroll = .distantPast
        let started = ContinuousClock.now
        await Fleet.scrollEnded()
        #expect(ContinuousClock.now - started < .milliseconds(50))
    }

    @Test func aChangeDuringAnEndlessScrollAppliesWithinTheHold() async throws {
        var (fleet, script) = try fleet()
        endlessScroll()
        defer { stop() }
        let renamed = SessionUpdate(events: [script.event(.titleChanged(title: "Renamed", source: .user))], streaming: [])
        let started = ContinuousClock.now
        #expect(await fleet.receive(renamed, for: script.key))
        let waited = ContinuousClock.now - started
        #expect(waited >= .milliseconds(450))
        #expect(waited < .seconds(5))
        #expect(fleet.session(script.key)?.title == "Renamed")
    }

    @Test func anApprovalDuringAScrollAppliesAtOnce() async throws {
        var (fleet, script) = try fleet()
        endlessScroll()
        defer { stop() }
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
