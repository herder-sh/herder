import CryptoKit
import Foundation
import Herder

/// The authenticator the app signs with: the platform's passkey; on the fake fleet of a test
/// run, the passkey of the person on it (`HERDER_FAKE_PASSKEY`, see `Fleet.joinFakeFleet`); in
/// a debug build launched with `-softwarePasskey` (what the UI tests pass), a software passkey,
/// as no simulator can answer Face ID for a test; or, in a copy of the Mac app that is not
/// signed by its team (the ad-hoc release zip), which macOS keeps from the platform's passkey,
/// this Mac's local passkey (`LocalPasskey`).
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
        return LocalPasskey()
    }
    return PlatformAuthenticator()
}

#if DEBUG
/// A passkey for herder.sh held in the app's defaults, which creates and signs exactly as a
/// platform authenticator encodes it, without Face ID: for UI tests only, so debug builds alone
/// have it, and only behind `-softwarePasskey`. The daemon's own test authenticator
/// (`herder_daemon::auth::authenticator`) encodes the same.
@MainActor final class SoftwarePasskey: Authenticator {
    static let argument = "-softwarePasskey"

    private let defaults: UserDefaults
    private let key: P256.Signing.PrivateKey

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        if let raw = defaults.data(forKey: "softwarePasskey.key"),
           let key = try? P256.Signing.PrivateKey(rawRepresentation: raw) {
            self.key = key
        } else {
            key = P256.Signing.PrivateKey()
            defaults.set(key.rawRepresentation, forKey: "softwarePasskey.key")
        }
    }

    private var credentialId: Data { SoftwareSigning.credentialId(key.publicKey) }

    /// The person the passkey was created for.
    private var userId: Data? {
        get { defaults.data(forKey: "softwarePasskey.user") }
        set { defaults.set(newValue, forKey: "softwarePasskey.user") }
    }

    func register(name: String, userId: Data, challenge: Data) async throws -> PasskeyRegistration {
        self.userId = userId
        return try SoftwareSigning.registration(publicKey: key.publicKey, credentialId: credentialId,
                                                challenge: challenge)
    }

    func assert(challenge: Data) async throws -> PasskeyAssertion {
        try SoftwareSigning.assertion(key: key, credentialId: credentialId, userHandle: userId, challenge: challenge)
    }
}
#endif

/// How a passkey the app holds itself creates and signs: registrations and assertions encoded
/// exactly as a platform authenticator encodes them, for the passkeys tests sign with in place
/// of Face ID and for a Mac's local passkey.
@MainActor enum SoftwareSigning {
    /// User present, user verified, backup eligible and backed up: a synced passkey.
    static let flags: UInt8 = 0x01 | 0x04 | 0x08 | 0x10

    /// Attested credential data follows.
    private static let attested: UInt8 = 0x40

    /// The credential id of a key: its public key's hash, so it stays with the key.
    static func credentialId(_ publicKey: P256.Signing.PublicKey) -> Data {
        Data(SHA256.hash(data: publicKey.rawRepresentation).prefix(16))
    }

    /// The registration of `publicKey` as the credential `credentialId`, with no attestation,
    /// answering `challenge`. `flags` are the authenticator data's, before the attested bit.
    static func registration(publicKey: P256.Signing.PublicKey, credentialId: Data, challenge: Data,
                             flags: UInt8 = SoftwareSigning.flags) throws -> PasskeyRegistration {
        var authData = authData(flags | attested)
        authData.append(Data(count: 16)) // No AAGUID.
        authData.append(contentsOf: [UInt8(credentialId.count >> 8), UInt8(credentialId.count & 0xff)])
        authData.append(credentialId)
        authData.append(coseKey(publicKey))
        var attestation = head(5, 3)
        attestation.append(text("fmt"))
        attestation.append(text("none"))
        attestation.append(text("attStmt"))
        attestation.append(head(5, 0))
        attestation.append(text("authData"))
        attestation.append(bytes(authData))
        let id = credentialId.base64URLEncodedString()
        return PasskeyRegistration(
            id: id, rawId: id,
            response: RegistrationResponse(
                clientDataJson: try clientData("webauthn.create", challenge).base64URLEncodedString(),
                attestationObject: attestation.base64URLEncodedString()),
            kind: "public-key")
    }

    /// `publicKey` as a COSE_Key: EC2, ES256, P-256, x, y.
    private static func coseKey(_ publicKey: P256.Signing.PublicKey) -> Data {
        let point = publicKey.rawRepresentation
        var cose = head(5, 5)
        cose.append(contentsOf: [0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21])
        cose.append(bytes(point.prefix(32)))
        cose.append(0x22)
        cose.append(bytes(point.suffix(32)))
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

    /// An assertion over `challenge` by `key`, the credential `credentialId` of the person
    /// `userHandle` names.
    static func assertion(key: P256.Signing.PrivateKey, credentialId: Data, userHandle: Data?,
                          challenge: Data) throws -> PasskeyAssertion {
        let message = try message(challenge)
        let signature = try key.signature(for: message.signed).derRepresentation
        return assertion(message, signature: signature, credentialId: credentialId, userHandle: userHandle)
    }

    /// What an assertion over `challenge` signs, `signed`, with the client and authenticator
    /// data it is made of. `flags` are the authenticator data's.
    static func message(_ challenge: Data, flags: UInt8 = SoftwareSigning.flags)
        throws -> (clientData: Data, authData: Data, signed: Data) {
        let clientData = try clientData("webauthn.get", challenge)
        let authData = authData(flags)
        return (clientData, authData, authData + SHA256.hash(data: clientData))
    }

    /// The assertion `signature`, DER, makes over `message`.
    static func assertion(_ message: (clientData: Data, authData: Data, signed: Data), signature: Data,
                          credentialId: Data, userHandle: Data?) -> PasskeyAssertion {
        let (clientData, authData, _) = message
        let id = credentialId.base64URLEncodedString()
        return PasskeyAssertion(
            id: id, rawId: id,
            response: AssertionResponse(
                clientDataJson: clientData.base64URLEncodedString(),
                authenticatorData: authData.base64URLEncodedString(),
                signature: signature.base64URLEncodedString(),
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
