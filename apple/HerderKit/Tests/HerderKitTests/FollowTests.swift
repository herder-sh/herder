import Foundation
import Herder
@testable import HerderKit
import Testing

/// The fleet takes in the client's lists whenever they change, which is many times a minute
/// with machines connected: reading them is not the main actor's work.
@MainActor struct FollowTests {
    /// What ran on the main actor, in order.
    private final class Steps {
        var taken: [String] = []
    }

    @Test func theMainActorRunsOtherWorkWhileTheListsAreRead() async throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CancellationError()
        }
        fleet.isScrolling = { false }
        let steps = Steps()
        // Waiting for the main actor before the change arrives, as a frame to draw would.
        let other = Task { steps.taken.append("other work") }
        await fleet.changed()
        steps.taken.append("lists taken in")
        await other.value
        #expect(steps.taken == ["other work", "lists taken in"])
    }
}
