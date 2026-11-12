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
}

extension Authenticator {
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

    func register(name: String, userId: Data, challenge: Data) async throws -> PasskeyRegistration {
        let request = provider.createCredentialRegistrationRequest(challenge: challenge, name: name, userID: userId)
        guard let credential = try await perform(request).credential
            as? ASAuthorizationPlatformPublicKeyCredentialRegistration,
            let attestation = credential.rawAttestationObject
        else { throw HerderError.Local(detail: "the passkey was not created") }
        let id = credential.credentialID.base64URLEncodedString()
        return PasskeyRegistration(
            id: id, rawId: id,
            response: RegistrationResponse(clientDataJson: credential.rawClientDataJSON.base64URLEncodedString(),
                                           attestationObject: attestation.base64URLEncodedString()),
            kind: "public-key")
    }

    func assert(challenge: Data, allowing credentials: [Data]) async throws -> PasskeyAssertion {
        let request = provider.createCredentialAssertionRequest(challenge: challenge)
        request.allowedCredentials = credentials.map {
            ASAuthorizationPlatformPublicKeyCredentialDescriptor(credentialID: $0)
        }
        guard let credential = try await perform(request).credential
            as? ASAuthorizationPlatformPublicKeyCredentialAssertion
        else { throw HerderError.Local(detail: "the passkey did not sign") }
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
