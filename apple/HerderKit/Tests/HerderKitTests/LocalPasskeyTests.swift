import CryptoKit
import Foundation
import Herder
@testable import HerderKit
import Testing

/// Keys held in memory in place of the Secure Enclave, which no test runner has: a handle is
/// the key's index. Signs unless told the person cancels.
final class MemoryKeys: LocalPasskeyKeys, @unchecked Sendable {
    private let lock = NSLock()
    private var keys: [P256.Signing.PrivateKey] = []
    private var reasons: [String] = []
    var cancels = false

    var asked: [String] { lock.withLock { reasons } }

    func create() throws -> Data {
        lock.withLock {
            keys.append(P256.Signing.PrivateKey())
            return Data([UInt8(keys.count - 1)])
        }
    }

    private func key(_ handle: Data) throws -> P256.Signing.PrivateKey {
        try lock.withLock {
            guard let index = handle.first, keys.indices.contains(Int(index)) else {
                throw HerderError.Local(detail: "no such key")
            }
            return keys[Int(index)]
        }
    }

    func publicKey(_ handle: Data) throws -> P256.Signing.PublicKey { try key(handle).publicKey }

    func sign(_ message: Data, with handle: Data, reason: String) async throws -> Data {
        lock.withLock { reasons.append(reason) }
        if cancels { throw CancellationError() }
        return try key(handle).signature(for: message).derRepresentation
    }
}

/// The local passkey of a Mac app that is not signed by its team.
@MainActor struct LocalPasskeyTests {
    private func defaults() throws -> UserDefaults {
        try #require(UserDefaults(suiteName: "local-passkey-\(UUID().uuidString)"))
    }

    @Test func signsWithTheKeyItRegistered() async throws {
        let keys = MemoryKeys()
        let passkey = LocalPasskey(defaults: try defaults(), keys: keys)
        let registration = try await passkey.register(name: "Owner", userId: Data([1, 2, 3]),
                                                      challenge: Data(count: 32))
        #expect(keys.asked.isEmpty)
        let assertion = try await passkey.assert(challenge: Data(repeating: 7, count: 32))
        #expect(keys.asked.count == 1)
        #expect(assertion.id == registration.id)
        #expect(assertion.response.userHandle == Data([1, 2, 3]).base64URLEncodedString())

        // What the machine checks: the signature over the authenticator data and the client
        // data's hash, by the registered key.
        let clientData = try #require(Data(base64URL: assertion.response.clientDataJson))
        let authData = try #require(Data(base64URL: assertion.response.authenticatorData))
        let signature = try P256.Signing.ECDSASignature(
            derRepresentation: try #require(Data(base64URL: assertion.response.signature)))
        let publicKey = try keys.publicKey(Data([0]))
        #expect(publicKey.isValidSignature(signature, for: authData + SHA256.hash(data: clientData)))
        // User present and verified; not a synced passkey.
        #expect(authData[32] == 0x05)
    }

    @Test func keepsOnlyTheHandleAndKeepsItAcrossLaunches() async throws {
        let (store, keys) = (try defaults(), MemoryKeys())
        let first = try await LocalPasskey(defaults: store, keys: keys).assert(challenge: Data(count: 32))
        let second = try await LocalPasskey(defaults: store, keys: keys).assert(challenge: Data(count: 32))
        #expect(first.id == second.id)
        #expect(store.data(forKey: "localPasskey.key") == Data([0]))
    }

    @Test func signsNothingWhenThePersonCancels() async throws {
        let keys = MemoryKeys()
        keys.cancels = true
        let passkey = LocalPasskey(defaults: try defaults(), keys: keys)
        do {
            _ = try await passkey.assert(challenge: Data(count: 32))
            Issue.record("signed without the person confirming")
        } catch {
            #expect(isCancelled(error))
        }
    }
}
