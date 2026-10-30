import Foundation
@testable import HerderKit
import Testing

/// The fleet's changes wait while a scroll is in progress.
@MainActor
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
    /// On the Mac a scroll is in progress until a moment after its last scroll-wheel event.
    @Test func aScrollEndsAMomentAfterItsLastEvent() async throws {
        Fleet.lastScroll = .now
        #expect(Fleet.scrolling)
        let started = Date.now
        await Fleet.scrollEnded()
        #expect(!Fleet.scrolling)
        #expect(Date.now.timeIntervalSince(started) >= Fleet.scrollSettles * 0.9)
    }
    #endif
}
