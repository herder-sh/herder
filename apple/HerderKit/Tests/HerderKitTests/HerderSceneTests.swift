import Foundation
@testable import HerderKit
import Testing

@MainActor
struct HerderSceneTests {
    /// SwiftUI makes the app's scene more than once. Each used to open the profile, and every
    /// profile opened is a client that connects to every machine: the daemons logged the app's
    /// connections in pairs.
    @Test func everySceneShowsTheOneProfileTheAppOpened() {
        // A profile of its own, named before the app's is first opened.
        setenv("HERDER_PROFILE", "scene-tests", 1)
        guard case .opened(let first) = HerderScene().profile,
              case .opened(let second) = HerderScene().profile else {
            Issue.record("the profile did not open")
            return
        }
        #expect(first === second)
    }
}
