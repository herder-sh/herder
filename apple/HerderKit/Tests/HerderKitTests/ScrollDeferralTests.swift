import Foundation
@testable import HerderKit
import Testing

/// The fleet's changes wait while a scroll is in progress: while the main run loop tracks one.
@MainActor
struct ScrollDeferralTests {
    private final class Seen: @unchecked Sendable { var value = false }

    @Test func aScrollIsInProgressWhileTheRunLoopTracks() {
        let during = Seen()
        RunLoop.main.perform(inModes: [Fleet.scrollMode]) {
            MainActor.assumeIsolated { during.value = Fleet.scrolling }
        }
        spin(Fleet.scrollMode)
        #expect(during.value)
        #expect(!Fleet.scrolling)
    }

    /// Runs the main run loop in `mode` for a moment, as UIKit does in its tracking mode for a
    /// scroll.
    private func spin(_ mode: RunLoop.Mode) {
        RunLoop.main.run(mode: mode, before: .now + 0.3)
    }
}
