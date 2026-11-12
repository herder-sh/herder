import AuthenticationServices
import Foundation
import Testing
@testable import HerderKit

/// The passkey requests ask for user verification, which every machine requires: left at
/// "preferred", 1Password signs without it while unlocked, and the machine refuses.
@MainActor
struct PlatformPasskeyTests {
    let platform = ASAuthorizationPlatformPublicKeyCredentialProvider(relyingPartyIdentifier: PlatformAuthenticator.relyingParty)
    let keys = ASAuthorizationSecurityKeyPublicKeyCredentialProvider(relyingPartyIdentifier: PlatformAuthenticator.relyingParty)

    @Test func signingRequiresUserVerification() {
        let request = PlatformAuthenticator.verified(platform.createCredentialAssertionRequest(challenge: Data([1])))
        #expect(request.userVerificationPreference == .required)
        let key = PlatformAuthenticator.verified(keys.createCredentialAssertionRequest(challenge: Data([1])))
        #expect(key.userVerificationPreference == .required)
    }

    @Test func creatingRequiresUserVerification() {
        let request = PlatformAuthenticator.verified(
            platform.createCredentialRegistrationRequest(challenge: Data([1]), name: "Tomas", userID: Data([7])))
        #expect(request.userVerificationPreference == .required)
        let key = PlatformAuthenticator.verified(
            keys.createCredentialRegistrationRequest(challenge: Data([1]), displayName: "Tomas", name: "Tomas", userID: Data([7])))
        #expect(key.userVerificationPreference == .required)
    }
}

/// Passkey prompts run one at a time, each once the one before has gone: AuthenticationServices
/// refuses a second request while one is up, and shows one made as the last sheet goes away as
/// a sheet nobody sees, which hangs for two minutes.
@MainActor
struct PasskeyPromptsTests {
    /// What each prompt and settle did, in order.
    @MainActor final class Log {
        var lines: [String] = []
        var firstEnds: CheckedContinuation<Void, Never>?
        var settled: CheckedContinuation<Void, Never>?
    }

    @Test func aSecondPromptWaitsForTheFirstToGo() async throws {
        let log = Log()
        let prompts = PasskeyPrompts(settle: {
            log.lines.append("settling")
            await withCheckedContinuation { log.settled = $0 }
            log.lines.append("gone")
        })
        let first = Task {
            try await prompts.run {
                log.lines.append("first up")
                await withCheckedContinuation { log.firstEnds = $0 }
                log.lines.append("first ends")
                return 1
            }
        }
        while log.firstEnds == nil { await Task.yield() }
        let second = Task {
            try await prompts.run {
                log.lines.append("second up")
                return 2
            }
        }
        for _ in 0..<20 { await Task.yield() }
        #expect(log.lines == ["first up"])

        log.firstEnds?.resume()
        #expect(try await first.value == 1)
        while log.settled == nil { await Task.yield() }
        for _ in 0..<20 { await Task.yield() }
        // The first's result is back at once, but the second waits for its sheet to go.
        #expect(log.lines == ["first up", "first ends", "settling"])

        log.settled?.resume()
        #expect(try await second.value == 2)
        #expect(log.lines == ["first up", "first ends", "settling", "gone", "second up"])
    }

    @Test func aFailedPromptStillLetsTheNextOneRun() async throws {
        let prompts = PasskeyPrompts(settle: {})
        await #expect(throws: ASAuthorizationError.self) {
            try await prompts.run { throw ASAuthorizationError(.canceled) }
        }
        #expect(try await prompts.run { "next" } == "next")
    }

    @Test func promptsTakeTheirTurnsInOrder() async throws {
        var order: [Int] = []
        let prompts = PasskeyPrompts(settle: {})
        let tasks = (1...3).map { n in Task { try await prompts.run { order.append(n) } } }
        for task in tasks { try await task.value }
        #expect(order == [1, 2, 3])
    }
}

/// The window a prompt shows over, and what a prompt that timed out says.
@MainActor
struct PasskeyPromptWindowTests {
    @Test func showsOverTheKeyWindowElseTheMainElseAVisibleOne() {
        #expect(PlatformAuthenticator.anchor(key: "key", main: "main", visible: ["a"]) == "key")
        // Right after another prompt closes, no window is key yet.
        #expect(PlatformAuthenticator.anchor(key: nil, main: "main", visible: ["a"]) == "main")
        #expect(PlatformAuthenticator.anchor(key: nil, main: nil, visible: ["a", "b"]) == "a")
        #expect(PlatformAuthenticator.anchor(key: String?.none, main: nil, visible: []) == nil)
    }

    @Test func aPromptTheSystemTimedOutSaysSo() {
        let cancelled = ASAuthorizationError(.canceled)
        let timedOut = PlatformAuthenticator.failure(cancelled, after: .seconds(120))
        #expect(describe(timedOut) == "the \(Biometry.name) prompt timed out; try again")
        #expect(!isCancelled(timedOut))
        // The person closing it is still a cancel, which nobody reports.
        #expect(isCancelled(PlatformAuthenticator.failure(cancelled, after: .seconds(4))))
        let refused = ASAuthorizationError(.failed)
        #expect((PlatformAuthenticator.failure(refused, after: .seconds(120)) as? ASAuthorizationError)?.code == .failed)
    }
}
