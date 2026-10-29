import Foundation
import Herder

/// This device's passkey as the client core signs a person's terminal and secret commands with:
/// whenever a machine asks for a fresh assertion (opening a terminal, attaching to one this
/// device has not had since the machine started, changing a secret), Face ID signs it, once per
/// action. Approving a terminal's request signs through it too.
final class FaceID: PasskeyAuthenticator {
    private let passkey: @MainActor @Sendable () -> Authenticator

    /// `passkey` makes the authenticator for each action; tests pass a fake.
    init(_ passkey: @escaping @MainActor @Sendable () -> Authenticator = { PlatformAuthenticator() }) {
        self.passkey = passkey
    }

    func assert(hostId: HostId, challenge: PasskeyChallenge) async throws -> PasskeyAssertion {
        try await sign(challenge)
    }

    @MainActor private func sign(_ challenge: PasskeyChallenge) async throws -> PasskeyAssertion {
        do {
            return try await passkey().sign(challenge)
        } catch {
            // The command that asked fails with this, as the terminal or secret's error.
            throw HerderError.Local(detail: isCancelled(error) ? "\(Biometry.name) was cancelled" : describe(error))
        }
    }
}
