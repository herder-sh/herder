import CryptoKit
import Foundation
import Herder

/// The passkey this app signs with: the platform's, or, launched with `HERDER_SOFTWARE_PASSKEY`
/// set, a software one, so UI tests approve and claim against fake daemons without Face ID.
enum Passkey {
    @MainActor static let device: Authenticator =
        ProcessInfo.processInfo.environment["HERDER_SOFTWARE_PASSKEY"] == nil ? PlatformAuthenticator() : SoftwareAuthenticator()
}

/// A passkey in memory for this launch, encoding registrations and assertions over a machine's
/// challenges exactly as a platform authenticator does, as the daemon's own test authenticator
/// does: a P-256 key, "none" attestation, the herder.sh origin.
@MainActor final class SoftwareAuthenticator: Authenticator {
    private let key = P256.Signing.PrivateKey()
    private let credentialId = Data((0..<16).map { _ in UInt8.random(in: 0...255) })
    private var userHandle = Data()

    /// User present, user verified, backup eligible and backed up: a synced passkey.
    private static let flags: UInt8 = 0x01 | 0x04 | 0x08 | 0x10
    /// Attested credential data follows.
    private static let attested: UInt8 = 0x40

    func register(name: String, userId: Data, challenge: Data) async throws -> PasskeyRegistration {
        userHandle = userId
        var authData = Self.authData(flags: Self.flags | Self.attested)
        authData += Data(count: 16) // No AAGUID.
        authData += withUnsafeBytes(of: UInt16(credentialId.count).bigEndian) { Data($0) }
        authData += credentialId
        authData += coseKey
        var attestation = Self.head(5, 3)
        attestation += Self.text("fmt") + Self.text("none")
        attestation += Self.text("attStmt") + Self.head(5, 0)
        attestation += Self.text("authData") + Self.bytes(authData)
        let id = credentialId.base64URLEncodedString()
        return PasskeyRegistration(
            id: id, rawId: id,
            response: RegistrationResponse(
                clientDataJson: try Self.clientData("webauthn.create", challenge).base64URLEncodedString(),
                attestationObject: attestation.base64URLEncodedString()),
            kind: "public-key")
    }

    func assert(challenge: Data) async throws -> PasskeyAssertion {
        let clientData = try Self.clientData("webauthn.get", challenge)
        let authData = Self.authData(flags: Self.flags)
        let signature = try key.signature(for: authData + Data(SHA256.hash(data: clientData)))
        let id = credentialId.base64URLEncodedString()
        return PasskeyAssertion(
            id: id, rawId: id,
            response: AssertionResponse(
                clientDataJson: clientData.base64URLEncodedString(),
                authenticatorData: authData.base64URLEncodedString(),
                signature: signature.derRepresentation.base64URLEncodedString(),
                userHandle: userHandle.base64URLEncodedString()),
            kind: "public-key")
    }

    /// The public key as a COSE_Key: EC2, ES256, P-256, x, y.
    private var coseKey: Data {
        let point = key.publicKey.x963Representation // 0x04, x, y.
        var cose = Self.head(5, 5)
        cose += Data([0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21])
        cose += Self.bytes(point[1..<33])
        cose += Data([0x22])
        cose += Self.bytes(point[33..<65])
        return cose
    }

    /// The RP id hash, `flags` and a zero signature counter, as synced passkeys keep it.
    private static func authData(flags: UInt8) -> Data {
        Data(SHA256.hash(data: Data(PlatformAuthenticator.relyingParty.utf8))) + Data([flags, 0, 0, 0, 0])
    }

    private static func clientData(_ type: String, _ challenge: Data) throws -> Data {
        try JSONSerialization.data(withJSONObject: [
            "type": type, "challenge": challenge.base64URLEncodedString(),
            "origin": "https://\(PlatformAuthenticator.relyingParty)", "crossOrigin": false,
        ] as [String: Any], options: [.sortedKeys, .withoutEscapingSlashes])
    }

    /// A CBOR head of `major` type with argument `count`.
    nonisolated static func head(_ major: UInt8, _ count: Int) -> Data {
        switch count {
        case ..<24: Data([major << 5 | UInt8(count)])
        case ..<256: Data([major << 5 | 24, UInt8(count)])
        default: Data([major << 5 | 25, UInt8(count >> 8), UInt8(count & 0xFF)])
        }
    }

    nonisolated static func bytes(_ value: Data) -> Data { head(2, value.count) + value }

    nonisolated static func text(_ value: String) -> Data { head(3, value.utf8.count) + Data(value.utf8) }
}
