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

    /// Every passkey prompt of the app, one at a time: see `PasskeyPrompts`.
    static let prompts = PasskeyPrompts()

    private let provider = ASAuthorizationPlatformPublicKeyCredentialProvider(relyingPartyIdentifier: relyingParty)
    private var pending: CheckedContinuation<ASAuthorization, Error>?
    /// The controller of the pending request, which AuthenticationServices does not retain.
    private var controller: ASAuthorizationController?

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
        return try await Self.prompts.run {
            let start = ContinuousClock.now
            do {
                return try await withCheckedThrowingContinuation { continuation in
                    pending = continuation
                    let controller = ASAuthorizationController(authorizationRequests: [request])
                    self.controller = controller
                    controller.delegate = self
                    controller.presentationContextProvider = self
                    controller.performRequests()
                }
            } catch {
                throw Self.failure(error, after: ContinuousClock.now - start)
            }
        }
    }

    /// How long the system leaves a passkey prompt up before it gives up on it: two minutes, as
    /// WebAuthn's default timeout. It then ends the request as cancelled, as if the person had
    /// closed it.
    static let systemTimeout = Duration.seconds(120)

    /// `error`, the request's after `elapsed`: one cancelled by the system's timeout rather
    /// than by the person says so, as nobody cancelled anything, and the prompt may never have
    /// shown.
    static func failure(_ error: Error, after elapsed: Duration) -> Error {
        // Nobody closes a prompt within a few seconds of its timeout by chance, so a cancel that
        // close to it is the timeout's; AuthenticationServices names no cause to tell them apart.
        guard isCancelled(error), elapsed >= systemTimeout - .seconds(10) else { return error }
        return HerderError.Local(detail: "the \(Biometry.name) prompt timed out; try again")
    }

    /// The window a passkey prompt shows over: the key one, else the main one, else any
    /// visible one. Right after another prompt closes, the app has no key window yet.
    static func anchor<Window>(key: Window?, main: Window?, visible: [Window]) -> Window? {
        key ?? main ?? visible.first
    }
}

/// The app's passkey prompts, run one at a time, each after the one before has gone.
///
/// AuthenticationServices refuses a request while another is up (`ASAuthorizationError.failed`),
/// and presents one made as the previous sheet is still going away as a sheet nobody sees,
/// which waits until the system times it out two minutes later. Two commands that each need a
/// fresh assertion, as storing a requested secret sends, ask for one right after the other.
@MainActor final class PasskeyPrompts {
    /// How long a prompt's sheet takes to go once its request has ended. The request ends as
    /// the sheet starts going away, in another process that tells the app nothing more, so no
    /// event marks it gone: waiting for the window to be key again misses the app not being
    /// frontmost. A second request 12 to 26 ms after the first got the invisible sheet; the
    /// dismissal animation takes about a third of a second, and this leaves margin over it.
    static let sheetGoes = Duration.milliseconds(600)

    private let settle: @MainActor () async -> Void
    private var busy = false
    /// The prompts waiting their turn, in the order they asked.
    private var waiting: [CheckedContinuation<Void, Never>] = []

    /// `settle` waits for an ended prompt's sheet to go; tests pass their own.
    init(settle: @escaping @MainActor () async -> Void = { try? await Task.sleep(for: PasskeyPrompts.sheetGoes) }) {
        self.settle = settle
    }

    /// Runs `prompt` once no other prompt is up nor still going away.
    func run<T>(_ prompt: () async throws -> T) async throws -> T {
        if busy {
            // `next` hands the turn over with `busy` still set, so no newcomer slips in.
            await withCheckedContinuation { waiting.append($0) }
        } else {
            busy = true
        }
        defer { Task { await settle(); next() } }
        return try await prompt()
    }

    private func next() {
        if waiting.isEmpty {
            busy = false
        } else {
            waiting.removeFirst().resume()
        }
    }
}

extension PlatformAuthenticator: ASAuthorizationControllerDelegate, ASAuthorizationControllerPresentationContextProviding {
    nonisolated func authorizationController(controller: ASAuthorizationController,
                                             didCompleteWithAuthorization authorization: ASAuthorization) {
        MainActor.assumeIsolated {
            pending?.resume(returning: authorization)
            pending = nil
            self.controller = nil
        }
    }

    nonisolated func authorizationController(controller: ASAuthorizationController, didCompleteWithError error: Error) {
        MainActor.assumeIsolated {
            pending?.resume(throwing: error)
            pending = nil
            self.controller = nil
        }
    }

    nonisolated func presentationAnchor(for controller: ASAuthorizationController) -> ASPresentationAnchor {
        MainActor.assumeIsolated {
            #if os(iOS)
            let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            let windows = scenes.sorted { $0.activationState == .foregroundActive && $1.activationState != .foregroundActive }
                .flatMap(\.windows)
            return PlatformAuthenticator.anchor(key: windows.first(where: \.isKeyWindow), main: nil,
                                                visible: windows.filter { !$0.isHidden }) ?? ASPresentationAnchor()
            #else
            let app = NSApplication.shared
            return PlatformAuthenticator.anchor(key: app.keyWindow, main: app.mainWindow,
                                                visible: app.orderedWindows.filter(\.isVisible)) ?? ASPresentationAnchor()
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
