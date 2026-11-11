import CryptoKit
import Foundation
import Herder
import LocalAuthentication

/// This Mac's local passkey for herder.sh: what a copy of the Mac app that is not signed by its
/// team (the ad-hoc release zip) signs with, as macOS keeps such a copy from the platform's
/// passkey and nobody could claim a machine or approve anything from it otherwise.
///
/// Its private key is made in the Secure Enclave and never leaves it: the app keeps only the
/// enclave's handle to it, which is useless on any other Mac, and the enclave signs only after
/// Touch ID or the Mac's password, each time. So it is bound to this Mac, where a platform
/// passkey syncs to your other devices: add another device from People to keep a way in.
///
/// It registers and signs exactly as a platform authenticator encodes them, with the flags of a
/// passkey that is not backed up.
@MainActor final class LocalPasskey: Authenticator {
    /// Where the passkey is kept, as the claim sheet names it.
    static let store = "this Mac's Secure Enclave"

    /// User present and user verified; neither backup eligible nor backed up.
    private static let flags: UInt8 = 0x01 | 0x04

    private let defaults: UserDefaults
    private let keys: LocalPasskeyKeys

    init(defaults: UserDefaults = .standard, keys: LocalPasskeyKeys = EnclaveKeys()) {
        self.defaults = defaults
        self.keys = keys
    }

    /// The handle of the passkey's key, made on first use.
    private func handle() throws -> Data {
        if let handle = defaults.data(forKey: "localPasskey.key") { return handle }
        let handle = try keys.create()
        defaults.set(handle, forKey: "localPasskey.key")
        return handle
    }

    /// The person the passkey was created for.
    private var userId: Data? {
        get { defaults.data(forKey: "localPasskey.user") }
        set { defaults.set(newValue, forKey: "localPasskey.user") }
    }

    func register(name: String, userId: Data, challenge: Data) async throws -> PasskeyRegistration {
        let publicKey = try keys.publicKey(try handle())
        self.userId = userId
        return try SoftwareSigning.registration(publicKey: publicKey,
                                                credentialId: SoftwareSigning.credentialId(publicKey),
                                                challenge: challenge, flags: Self.flags)
    }

    func assert(challenge: Data) async throws -> PasskeyAssertion {
        let handle = try handle()
        let publicKey = try keys.publicKey(handle)
        let message = try SoftwareSigning.message(challenge, flags: Self.flags)
        let signature = try await keys.sign(message.signed, with: handle, reason: "approve with your herder passkey")
        return SoftwareSigning.assertion(message, signature: signature,
                                         credentialId: SoftwareSigning.credentialId(publicKey), userHandle: userId)
    }
}

/// Where a local passkey's private key lives. A key is known by its handle: what the app may
/// keep on disk, which must not be the key itself.
protocol LocalPasskeyKeys: Sendable {
    /// Makes a key, returning its handle.
    func create() throws -> Data
    /// The public key of the key `handle` names.
    func publicKey(_ handle: Data) throws -> P256.Signing.PublicKey
    /// Signs `message` with it, DER, once the person has shown it is them; throws
    /// `CancellationError` when they close the prompt, which says `reason`.
    func sign(_ message: Data, with handle: Data, reason: String) async throws -> Data
}

/// Keys in the Secure Enclave, which signs with one only after Touch ID or the Mac's password.
struct EnclaveKeys: LocalPasskeyKeys {
    func create() throws -> Data {
        guard SecureEnclave.isAvailable else {
            throw HerderError.Local(detail: "this Mac has no Secure Enclave to keep a local passkey in")
        }
        var failure: Unmanaged<CFError>?
        guard let access = SecAccessControlCreateWithFlags(
            nil, kSecAttrAccessibleWhenUnlockedThisDeviceOnly, [.privateKeyUsage, .userPresence], &failure)
        else {
            let reason = failure.map { describe($0.takeRetainedValue() as Error) } ?? "no access control"
            throw HerderError.Local(detail: "the local passkey could not be protected: \(reason)")
        }
        return try SecureEnclave.P256.Signing.PrivateKey(accessControl: access).dataRepresentation
    }

    func publicKey(_ handle: Data) throws -> P256.Signing.PublicKey {
        try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: handle).publicKey
    }

    func sign(_ message: Data, with handle: Data, reason: String) async throws -> Data {
        // Off the main actor: signing waits for the person to answer the system's prompt.
        try await Task.detached {
            let context = LAContext()
            context.localizedReason = reason
            do {
                let key = try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: handle,
                                                                    authenticationContext: context)
                return try key.signature(for: message).derRepresentation
            } catch where Self.isCancelled(error) {
                throw CancellationError()
            }
        }.value
    }

    /// Whether `error` is the person closing the Touch ID or password prompt.
    private static func isCancelled(_ error: Error) -> Bool {
        let error = error as NSError
        let cancelled: [LAError.Code] = [.userCancel, .appCancel, .systemCancel]
        return (error.domain == LAError.errorDomain && cancelled.contains { $0.rawValue == error.code })
            || (error.domain == NSOSStatusErrorDomain && error.code == Int(errSecUserCanceled))
    }
}
