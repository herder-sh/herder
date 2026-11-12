import AuthenticationServices
import Foundation
import Herder
import Security

/// Your herder passkey, for herder.sh: kept in iCloud Keychain or your password manager, its
/// private key never leaving them. Machines store only its public key; each approval signs a
/// challenge with it, behind Face ID.
@MainActor protocol Authenticator {
    /// Creates the passkey for `name`, returning what the machines store, as WebAuthn JSON.
    func register(name: String, userId: Data, challenge: Data) async throws -> PasskeyRegistration
    /// Signs `challenge` with the passkey, after Face ID or Touch ID, as WebAuthn JSON.
    func assert(challenge: Data) async throws -> PasskeyAssertion
    /// Creates another passkey of yours, in a password manager or on a security key.
    func register(name: String, userId: Data, challenge: Data, on place: PasskeyPlace) async throws -> PasskeyRegistration
    /// Signs `challenge` with the passkey `credentialId` alone, where `place` keeps it.
    func assert(challenge: Data, credentialId: Data, on place: PasskeyPlace) async throws -> PasskeyAssertion
}

/// Where Add a passkey keeps the new one: a password manager the system offers (Apple
/// Passwords, 1Password, Bitwarden…), or a security key such as a YubiKey.
enum PasskeyPlace: Hashable {
    case passwordManager
    case securityKey
}

extension Authenticator {
    /// Authenticators with no security keys, such as the test ones, keep every passkey as their own.
    func register(name: String, userId: Data, challenge: Data, on place: PasskeyPlace) async throws -> PasskeyRegistration {
        try await register(name: name, userId: userId, challenge: challenge)
    }

    func assert(challenge: Data, credentialId: Data, on place: PasskeyPlace) async throws -> PasskeyAssertion {
        try await assert(challenge: challenge)
    }

    /// Signs a machine's challenge: what People and the approval sheets sign with.
    func sign(_ challenge: PasskeyChallenge) async throws -> PasskeyAssertion {
        guard let bytes = Data(base64URL: challenge.challenge) else {
            throw HerderError.Local(detail: "the machine sent a challenge that is not base64url")
        }
        return try await assert(challenge: bytes)
    }
}

/// The passkey of the device's platform authenticator, through AuthenticationServices.
@MainActor final class PlatformAuthenticator: NSObject, Authenticator {
    static let relyingParty = "herder.sh"

    private let provider = ASAuthorizationPlatformPublicKeyCredentialProvider(relyingPartyIdentifier: relyingParty)
    private var pending: CheckedContinuation<ASAuthorization, Error>?

    private let securityKeys = ASAuthorizationSecurityKeyPublicKeyCredentialProvider(relyingPartyIdentifier: relyingParty)

    func register(name: String, userId: Data, challenge: Data) async throws -> PasskeyRegistration {
        let registration = try await register(name: name, userId: userId, challenge: challenge, on: .passwordManager)
        ThisDevicePasskey.remember(registration.id)
        return registration
    }

    func assert(challenge: Data) async throws -> PasskeyAssertion {
        let request = provider.createCredentialAssertionRequest(challenge: challenge)
        guard let credential = try await perform(request).credential
            as? ASAuthorizationPlatformPublicKeyCredentialAssertion
        else { throw HerderError.Local(detail: "the passkey did not sign") }
        let assertion = Self.assertion(credential)
        ThisDevicePasskey.remember(assertion.id)
        return assertion
    }

    func register(name: String, userId: Data, challenge: Data, on place: PasskeyPlace) async throws -> PasskeyRegistration {
        let request: ASAuthorizationRequest
        switch place {
        case .passwordManager:
            request = provider.createCredentialRegistrationRequest(challenge: challenge, name: name, userID: userId)
        case .securityKey:
            let key = securityKeys.createCredentialRegistrationRequest(challenge: challenge, displayName: name, name: name,
                                                                       userID: userId)
            key.credentialParameters = [ASAuthorizationPublicKeyCredentialParameters(algorithm: .ES256)]
            // Machines sign you in without naming you first, so the key must hold who you are.
            key.residentKeyPreference = .required
            request = key
        }
        let credential = try await perform(request).credential
        if let made = credential as? ASAuthorizationPlatformPublicKeyCredentialRegistration, let attestation = made.rawAttestationObject {
            return Self.registration(id: made.credentialID, clientData: made.rawClientDataJSON, attestation: attestation)
        }
        if let made = credential as? ASAuthorizationSecurityKeyPublicKeyCredentialRegistration, let attestation = made.rawAttestationObject {
            return Self.registration(id: made.credentialID, clientData: made.rawClientDataJSON, attestation: attestation)
        }
        throw HerderError.Local(detail: "the passkey was not created")
    }

    func assert(challenge: Data, credentialId: Data, on place: PasskeyPlace) async throws -> PasskeyAssertion {
        let request: ASAuthorizationRequest
        switch place {
        case .passwordManager:
            let platform = provider.createCredentialAssertionRequest(challenge: challenge)
            platform.allowedCredentials = [ASAuthorizationPlatformPublicKeyCredentialDescriptor(credentialID: credentialId)]
            request = platform
        case .securityKey:
            let key = securityKeys.createCredentialAssertionRequest(challenge: challenge)
            key.allowedCredentials = [ASAuthorizationSecurityKeyPublicKeyCredentialDescriptor(
                credentialID: credentialId, transports: ASAuthorizationSecurityKeyPublicKeyCredentialDescriptor.Transport.allSupported)]
            request = key
        }
        let credential = try await perform(request).credential
        if let signed = credential as? ASAuthorizationPlatformPublicKeyCredentialAssertion {
            return Self.assertion(signed)
        }
        if let signed = credential as? ASAuthorizationSecurityKeyPublicKeyCredentialAssertion {
            return Self.assertion(signed)
        }
        throw HerderError.Local(detail: "the passkey did not sign")
    }

    private static func registration(id: Data, clientData: Data, attestation: Data) -> PasskeyRegistration {
        let id = id.base64URLEncodedString()
        return PasskeyRegistration(
            id: id, rawId: id,
            response: RegistrationResponse(clientDataJson: clientData.base64URLEncodedString(),
                                           attestationObject: attestation.base64URLEncodedString()),
            kind: "public-key")
    }

    private static func assertion(_ credential: some ASAuthorizationPublicKeyCredentialAssertion) -> PasskeyAssertion {
        let id = credential.credentialID.base64URLEncodedString()
        return PasskeyAssertion(
            id: id, rawId: id,
            response: AssertionResponse(
                clientDataJson: credential.rawClientDataJSON.base64URLEncodedString(),
                authenticatorData: credential.rawAuthenticatorData.base64URLEncodedString(),
                signature: credential.signature.base64URLEncodedString(),
                userHandle: credential.userID.base64URLEncodedString()),
            kind: "public-key")
    }

    private func perform(_ request: ASAuthorizationRequest) async throws -> ASAuthorization {
        #if os(macOS)
        // An ad-hoc signed app has no application identifier nor associated domains, and macOS
        // fails its passkey requests with an error that names neither.
        let task = SecTaskCreateFromSelf(nil)
        if task.flatMap({ SecTaskCopyValueForEntitlement($0, "com.apple.developer.associated-domains" as CFString, nil) }) == nil {
            throw HerderError.Local(detail: """
                this copy of herder is not signed by its team, so macOS keeps it from your passkey: \
                install a team-signed build, such as apple/scripts/install-mac.sh builds
                """)
        }
        #endif
        return try await withCheckedThrowingContinuation { continuation in
            pending = continuation
            let controller = ASAuthorizationController(authorizationRequests: [request])
            controller.delegate = self
            controller.presentationContextProvider = self
            controller.performRequests()
        }
    }
}

extension PlatformAuthenticator: ASAuthorizationControllerDelegate, ASAuthorizationControllerPresentationContextProviding {
    nonisolated func authorizationController(controller: ASAuthorizationController,
                                             didCompleteWithAuthorization authorization: ASAuthorization) {
        MainActor.assumeIsolated {
            pending?.resume(returning: authorization)
            pending = nil
        }
    }

    nonisolated func authorizationController(controller: ASAuthorizationController, didCompleteWithError error: Error) {
        MainActor.assumeIsolated {
            pending?.resume(throwing: error)
            pending = nil
        }
    }

    nonisolated func presentationAnchor(for controller: ASAuthorizationController) -> ASPresentationAnchor {
        MainActor.assumeIsolated {
            #if os(iOS)
            let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            return scenes.flatMap(\.windows).first(where: \.isKeyWindow) ?? ASPresentationAnchor()
            #else
            return NSApplication.shared.keyWindow ?? ASPresentationAnchor()
            #endif
        }
    }
}

/// The passkey this device last signed with, by its credential id: the one the You page says
/// "this device uses". A passkey made for somewhere else, as Add a passkey makes, is not it.
enum ThisDevicePasskey {
    static let key = "passkey.thisDevice"

    static var credentialId: String? { UserDefaults.standard.string(forKey: key) }

    static func remember(_ credentialId: String) {
        UserDefaults.standard.set(credentialId, forKey: key)
    }
}

/// Whether `error` is the person closing the Face ID prompt, which is no failure to report.
func isCancelled(_ error: Error) -> Bool {
    (error as? ASAuthorizationError)?.code == .canceled
}

extension Data {
    /// Base64url without padding, as WebAuthn JSON encodes bytes.
    func base64URLEncodedString() -> String {
        base64EncodedString().replacing("+", with: "-").replacing("/", with: "_").replacing("=", with: "")
    }

    init?(base64URL: String) {
        var text = base64URL.replacing("-", with: "+").replacing("_", with: "/")
        text += String(repeating: "=", count: (4 - text.count % 4) % 4)
        self.init(base64Encoded: text)
    }
}
