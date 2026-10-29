import CryptoKit
import Foundation
import Herder

/// The authenticator the app signs with: the platform's passkey; on the fake fleet of a test
/// run, the passkey of the person on it (`HERDER_FAKE_PASSKEY`, see `Fleet.joinFakeFleet`); or,
/// in a debug build launched with `-softwarePasskey` (what the UI tests pass), a software
/// passkey, as no simulator can answer Face ID for a test.
@MainActor func passkeyAuthenticator() -> Authenticator {
    if let passkey = ProcessInfo.processInfo.environment["HERDER_FAKE_PASSKEY"].flatMap(RecordedPasskey.init) {
        return passkey
    }
    #if DEBUG
    if ProcessInfo.processInfo.arguments.contains(SoftwarePasskey.argument) {
        return SoftwarePasskey()
    }
    #endif
    return PlatformAuthenticator()
}

#if DEBUG
/// A passkey for herder.sh held in the app's defaults, which creates and signs exactly as a
/// platform authenticator encodes it, without Face ID: for UI tests only, so debug builds alone
/// have it, and only behind `-softwarePasskey`. The daemon's own test authenticator
/// (`herder_daemon::auth::authenticator`) encodes the same.
@MainActor final class SoftwarePasskey: Authenticator {
    static let argument = "-softwarePasskey"

    /// Attested credential data follows.
    private static let attested: UInt8 = 0x40

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

    /// The credential id: the public key's hash, so it stays with the key.
    private var credentialId: Data { Data(SHA256.hash(data: key.publicKey.rawRepresentation).prefix(16)) }

    /// The person the passkey was created for.
    private var userId: Data? {
        get { defaults.data(forKey: "softwarePasskey.user") }
        set { defaults.set(newValue, forKey: "softwarePasskey.user") }
    }

    func register(name: String, userId: Data, challenge: Data) async throws -> PasskeyRegistration {
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
        try SoftwareSigning.assertion(key: key, credentialId: credentialId, userHandle: userId, challenge: challenge)
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
#endif

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
