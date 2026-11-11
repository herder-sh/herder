import CryptoKit
import Foundation
import Herder
import LocalAuthentication

/// The authenticator the app signs with: the platform's passkey; on the fake fleet of a test
/// run, the passkey of the person on it (`HERDER_FAKE_PASSKEY`, see `Fleet.joinFakeFleet`); in
/// a debug build launched with `-softwarePasskey` (what the UI tests pass), a software passkey,
/// as no simulator can answer Face ID for a test; or, in a copy of the Mac app that is not
/// signed by its team (the ad-hoc release zip), which macOS keeps from the platform's passkey,
/// a local passkey: the same software passkey behind Touch ID or the Mac's password.
@MainActor func passkeyAuthenticator() -> Authenticator {
    if let passkey = ProcessInfo.processInfo.environment["HERDER_FAKE_PASSKEY"].flatMap(RecordedPasskey.init) {
        return passkey
    }
    #if DEBUG
    if ProcessInfo.processInfo.arguments.contains(SoftwarePasskey.argument) {
        return SoftwarePasskey()
    }
    #endif
    if !PlatformAuthenticator.isAvailable {
        return SoftwarePasskey(confirm: SoftwarePasskey.deviceOwner)
    }
    return PlatformAuthenticator()
}

/// A passkey for herder.sh held in the app's defaults, which creates and signs exactly as a
/// platform authenticator encodes it. The daemon's own test authenticator
/// (`herder_daemon::auth::authenticator`) encodes the same.
///
/// It has two uses. UI tests sign with it without Face ID, in debug builds behind
/// `-softwarePasskey`. And a Mac app that is not signed by its team signs with it as its local
/// passkey, after `confirm` (Touch ID or the Mac's password): macOS gives such a copy no
/// platform passkey, so without it nobody could claim a machine or approve anything from the
/// release zip. A local passkey stays on this Mac, in this app's container: it is not synced,
/// and is only as safe as the account it lives in. Add another device from People to keep a
/// way in.
@MainActor final class SoftwarePasskey: Authenticator {
    static let argument = "-softwarePasskey"

    /// Where a local passkey is kept, as the claim sheet names it.
    static let store = "this Mac only"

    /// Attested credential data follows.
    private static let attested: UInt8 = 0x40

    private let defaults: UserDefaults
    private let key: P256.Signing.PrivateKey
    /// Asks the person to confirm it is them before the key is used; throws when they do not.
    private let confirm: (@MainActor (String) async throws -> Void)?

    init(defaults: UserDefaults = .standard, confirm: (@MainActor (String) async throws -> Void)? = nil) {
        self.defaults = defaults
        self.confirm = confirm
        if let raw = defaults.data(forKey: "softwarePasskey.key"),
           let key = try? P256.Signing.PrivateKey(rawRepresentation: raw) {
            self.key = key
        } else {
            key = P256.Signing.PrivateKey()
            defaults.set(key.rawRepresentation, forKey: "softwarePasskey.key")
        }
    }

    /// The credential id: the public key's hash, so it stays with the key.
    private var credentialId: Data { Data(SHA256.hash(data: key.publicKey.rawRepresentation).prefix(16)) }

    /// The person the passkey was created for.
    private var userId: Data? {
        get { defaults.data(forKey: "softwarePasskey.user") }
        set { defaults.set(newValue, forKey: "softwarePasskey.user") }
    }

    /// Touch ID, or the Mac's password where there is none, as the system asks for them.
    static func deviceOwner(_ reason: String) async throws {
        let context = LAContext()
        var unavailable: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &unavailable) else {
            throw HerderError.Local(detail: """
                this Mac has no Touch ID nor password to approve with: \
                \(unavailable?.localizedDescription ?? "set a login password")
                """)
        }
        // The reply comes on a queue of its own; only the continuation crosses to it.
        try await withCheckedThrowingContinuation { (done: CheckedContinuation<Void, Error>) in
            context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason) { @Sendable approved, error in
                if approved {
                    done.resume()
                } else if let error = error as? LAError, [.userCancel, .appCancel, .systemCancel].contains(error.code) {
                    done.resume(throwing: CancellationError())
                } else {
                    done.resume(throwing: error ?? HerderError.Local(detail: "this Mac did not confirm it is you"))
                }
            }
        }
    }

    func register(name: String, userId: Data, challenge: Data) async throws -> PasskeyRegistration {
        try await confirm?("create your herder passkey on this Mac")
        self.userId = userId
        var authData = SoftwareSigning.authData(SoftwareSigning.flags | Self.attested)
        authData.append(Data(count: 16)) // No AAGUID.
        authData.append(contentsOf: [UInt8(credentialId.count >> 8), UInt8(credentialId.count & 0xff)])
        authData.append(credentialId)
        authData.append(coseKey)
        var attestation = Self.head(5, 3)
        attestation.append(Self.text("fmt"))
        attestation.append(Self.text("none"))
        attestation.append(Self.text("attStmt"))
        attestation.append(Self.head(5, 0))
        attestation.append(Self.text("authData"))
        attestation.append(Self.bytes(authData))
        let id = credentialId.base64URLEncodedString()
        return PasskeyRegistration(
            id: id, rawId: id,
            response: RegistrationResponse(
                clientDataJson: try SoftwareSigning.clientData("webauthn.create", challenge).base64URLEncodedString(),
                attestationObject: attestation.base64URLEncodedString()),
            kind: "public-key")
    }

    func assert(challenge: Data) async throws -> PasskeyAssertion {
        try await confirm?("approve with your herder passkey")
        return try SoftwareSigning.assertion(key: key, credentialId: credentialId, userHandle: userId,
                                             challenge: challenge)
    }

    /// The public key as a COSE_Key: EC2, ES256, P-256, x, y.
    private var coseKey: Data {
        let point = key.publicKey.rawRepresentation
        var cose = Self.head(5, 5)
        cose.append(contentsOf: [0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21])
        cose.append(Self.bytes(point.prefix(32)))
        cose.append(0x22)
        cose.append(Self.bytes(point.suffix(32)))
        return cose
    }

    /// A CBOR head of `major` type with argument `count`.
    private static func head(_ major: UInt8, _ count: Int) -> Data {
        switch count {
        case ..<24: Data([major << 5 | UInt8(count)])
        case ..<256: Data([major << 5 | 24, UInt8(count)])
        default: Data([major << 5 | 25, UInt8(count >> 8), UInt8(count & 0xff)])
        }
    }

    private static func bytes(_ value: Data) -> Data { head(2, value.count) + value }

    private static func text(_ value: String) -> Data { head(3, value.utf8.count) + Data(value.utf8) }
}

/// How a passkey held in software signs: assertions encoded exactly as a platform
/// authenticator encodes them, for the passkeys tests sign with in place of Face ID.
@MainActor enum SoftwareSigning {
    /// User present, user verified, backup eligible and backed up: a synced passkey.
    static let flags: UInt8 = 0x01 | 0x04 | 0x08 | 0x10

    /// An assertion over `challenge` by `key`, the credential `credentialId` of the person
    /// `userHandle` names.
    static func assertion(key: P256.Signing.PrivateKey, credentialId: Data, userHandle: Data?,
                          challenge: Data) throws -> PasskeyAssertion {
        let clientData = try clientData("webauthn.get", challenge)
        let authData = authData(flags)
        let signature = try key.signature(for: authData + SHA256.hash(data: clientData))
        let id = credentialId.base64URLEncodedString()
        return PasskeyAssertion(
            id: id, rawId: id,
            response: AssertionResponse(
                clientDataJson: clientData.base64URLEncodedString(),
                authenticatorData: authData.base64URLEncodedString(),
                signature: signature.derRepresentation.base64URLEncodedString(),
                userHandle: userHandle?.base64URLEncodedString()),
            kind: "public-key")
    }

    /// The RP id hash, `flags` and a zero signature counter, as synced passkeys keep it.
    static func authData(_ flags: UInt8) -> Data {
        var data = Data(SHA256.hash(data: Data(PlatformAuthenticator.relyingParty.utf8)))
        data.append(flags)
        data.append(Data(count: 4))
        return data
    }

    static func clientData(_ kind: String, _ challenge: Data) throws -> Data {
        try JSONSerialization.data(withJSONObject: [
            "type": kind,
            "challenge": challenge.base64URLEncodedString(),
            "origin": "https://\(PlatformAuthenticator.relyingParty)",
            "crossOrigin": false,
        ] as [String: Any])
    }
}
