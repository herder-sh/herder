import AuthenticationServices
import Foundation
import Herder

/// Your herder passkey, for herder.sh: kept in iCloud Keychain or your password manager, its
/// private key never leaving them. Machines store only its public key; each approval signs a
/// challenge with it, behind Face ID.
@MainActor protocol Authenticator {
    /// Creates the passkey for `name`, returning what the machines store.
    func register(name: String, userId: Data, challenge: Data) async throws -> PasskeyRegistration
    /// Signs `challenge` with the passkey, after Face ID or Touch ID.
    func assert(challenge: Data) async throws -> PasskeyAssertion
}

struct PasskeyRegistration: Equatable {
    let credentialId: Data
    /// The attestation object, holding the public key.
    let attestation: Data
    let clientData: Data
}

struct PasskeyAssertion: Equatable {
    let credentialId: Data
    let authenticatorData: Data
    let signature: Data
    let clientData: Data
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
        return PasskeyRegistration(credentialId: credential.credentialID, attestation: attestation,
                                   clientData: credential.rawClientDataJSON)
    }

    func assert(challenge: Data) async throws -> PasskeyAssertion {
        let request = provider.createCredentialAssertionRequest(challenge: challenge)
        guard let credential = try await perform(request).credential
            as? ASAuthorizationPlatformPublicKeyCredentialAssertion
        else { throw HerderError.Local(detail: "the passkey did not sign") }
        return PasskeyAssertion(credentialId: credential.credentialID, authenticatorData: credential.rawAuthenticatorData,
                                signature: credential.signature, clientData: credential.rawClientDataJSON)
    }

    private func perform(_ request: ASAuthorizationRequest) async throws -> ASAuthorization {
        try await withCheckedThrowingContinuation { continuation in
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
