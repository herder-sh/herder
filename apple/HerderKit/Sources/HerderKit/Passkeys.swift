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
    /// Signs `challenge` with the passkey, after Face ID or Touch ID, as WebAuthn JSON: with one
    /// of `credentials`, by credential id, or with any passkey for herder.sh when it is empty.
    func assert(challenge: Data, allowing credentials: [Data]) async throws -> PasskeyAssertion
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
        try await assert(challenge: challenge, allowing: [credentialId])
    }

    /// Signs `challenge` with any passkey for herder.sh.
    func assert(challenge: Data) async throws -> PasskeyAssertion {
        try await assert(challenge: challenge, allowing: [])
    }

    /// Signs a machine's challenge, with one of the passkeys it allows: what People, the
    /// approval sheets and signing in from another device's link sign with.
    func sign(_ challenge: PasskeyChallenge) async throws -> PasskeyAssertion {
        guard let bytes = Data(base64URL: challenge.challenge) else {
            throw HerderError.Local(detail: "the machine sent a challenge that is not base64url")
        }
        let credentials = challenge.allowCredentials.compactMap { Data(base64URL: $0) }
        guard credentials.count == challenge.allowCredentials.count else {
            throw HerderError.Local(detail: "a passkey to sign with is not base64url")
        }
        return try await assert(challenge: bytes, allowing: credentials)
    }
}

/// The error of an authenticator whose passkey `credentialId` is not among `credentials`, as
/// a platform offers no passkey then; nil when it is, or any passkey will do.
func notAllowed(_ credentialId: Data, _ credentials: [Data]) -> HerderError? {
    credentials.isEmpty || credentials.contains(credentialId)
        ? nil : HerderError.Local(detail: "this passkey is not one the link allows")
}

/// The passkey of the device's platform authenticator, through AuthenticationServices.
@MainActor final class PlatformAuthenticator: NSObject, Authenticator {
    static let relyingParty = "herder.sh"

    private let provider = ASAuthorizationPlatformPublicKeyCredentialProvider(relyingPartyIdentifier: relyingParty)
    private var pending: CheckedContinuation<ASAuthorization, Error>?

    private let securityKeys = ASAuthorizationSecurityKeyPublicKeyCredentialProvider(relyingPartyIdentifier: relyingParty)

    // Every machine requires user verification (Face ID, Touch ID, the passcode or the key's
    // PIN). Left at its default, "preferred", a password manager such as 1Password signs
    // without it while unlocked, and the machine refuses the assertion.

    /// `request`, asking for user verification.
    static func verified<Request: ASAuthorizationPublicKeyCredentialAssertionRequest>(_ request: Request) -> Request {
        request.userVerificationPreference = .required
        return request
    }

    /// `request`, asking for user verification.
    static func verified<Request: ASAuthorizationPublicKeyCredentialRegistrationRequest>(_ request: Request) -> Request {
        request.userVerificationPreference = .required
        return request
    }

    func register(name: String, userId: Data, challenge: Data) async throws -> PasskeyRegistration {
        let registration = try await register(name: name, userId: userId, challenge: challenge, on: .passwordManager)
        ThisDevicePasskey.remember(registration.id)
        return registration
    }

    func assert(challenge: Data, allowing credentials: [Data]) async throws -> PasskeyAssertion {
        let request = Self.verified(provider.createCredentialAssertionRequest(challenge: challenge))
        request.allowedCredentials = credentials.map {
            ASAuthorizationPlatformPublicKeyCredentialDescriptor(credentialID: $0)
        }
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
            request = Self.verified(provider.createCredentialRegistrationRequest(challenge: challenge, name: name, userID: userId))
        case .securityKey:
            let key = Self.verified(securityKeys.createCredentialRegistrationRequest(challenge: challenge, displayName: name,
                                                                                     name: name, userID: userId))
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
            let platform = Self.verified(provider.createCredentialAssertionRequest(challenge: challenge))
            platform.allowedCredentials = [ASAuthorizationPlatformPublicKeyCredentialDescriptor(credentialID: credentialId)]
            request = platform
        case .securityKey:
            let key = Self.verified(securityKeys.createCredentialAssertionRequest(challenge: challenge))
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

/// Where a passkey is kept, by the AAGUID of the authenticator that made it: "Apple Passwords",
/// "1Password". Nil for one that gave none, or one of a password manager not listed here.
enum PasskeyHome {
    /// The AAGUIDs password managers register their passkeys with, as the community list
    /// (passkeydeveloper/passkey-authenticator-aaguids) names them.
    static let names: [String: String] = [
        "fbfc3007-154e-4ecc-8c0b-6e020557d7bd": "Apple Passwords",
        "dd4ec289-e01d-41c9-bb89-70fa845d4bf2": "Apple Passwords",
        "bada5566-a7aa-401f-bd96-45619a55120d": "1Password",
        "ea9b8d66-4d01-1d21-3ce4-b6b48cb575d4": "Google Password Manager",
        "d548826e-79b4-db40-a3d8-11116f7e8349": "Bitwarden",
        "531126d6-e717-415c-9320-3d9aa6981239": "Dashlane",
        "50726f74-6f6e-5061-7373-50726f746f6e": "Proton Pass",
        "08987058-cadc-4b81-b6e1-30de50dcbe96": "Windows Hello",
    ]

    static func name(aaguid: String?) -> String? {
        aaguid.flatMap { names[$0.lowercased()] }
    }
}
