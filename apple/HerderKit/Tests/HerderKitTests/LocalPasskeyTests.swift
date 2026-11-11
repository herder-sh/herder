import Foundation
@testable import HerderKit
import Testing

/// The local passkey of a Mac app that is not signed by its team: the software passkey, used
/// only once the person confirms it is them.
@MainActor struct LocalPasskeyTests {
    private func defaults() throws -> UserDefaults {
        try #require(UserDefaults(suiteName: "local-passkey-\(UUID().uuidString)"))
    }

    @Test func asksBeforeCreatingAndBeforeEachSignature() async throws {
        var asked = 0
        let passkey = SoftwarePasskey(defaults: try defaults()) { _ in asked += 1 }
        let registration = try await passkey.register(name: "Owner", userId: Data([1, 2, 3]),
                                                      challenge: Data(count: 32))
        #expect(asked == 1)
        let assertion = try await passkey.assert(challenge: Data(count: 32))
        #expect(asked == 2)
        #expect(assertion.id == registration.id)
    }

    @Test func signsNothingWhenThePersonCancels() async throws {
        let passkey = SoftwarePasskey(defaults: try defaults()) { _ in throw CancellationError() }
        do {
            _ = try await passkey.assert(challenge: Data(count: 32))
            Issue.record("signed without the person confirming")
        } catch {
            #expect(isCancelled(error))
        }
    }

    @Test func keepsItsKeyAcrossLaunches() async throws {
        let store = try defaults()
        let first = try await SoftwarePasskey(defaults: store).assert(challenge: Data(count: 32))
        let second = try await SoftwarePasskey(defaults: store).assert(challenge: Data(count: 32))
        #expect(first.id == second.id)
    }
}
