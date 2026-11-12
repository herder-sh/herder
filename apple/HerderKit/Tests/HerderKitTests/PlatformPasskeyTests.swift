import AuthenticationServices
import Foundation
import Testing
@testable import HerderKit

/// The passkey requests ask for user verification, which every machine requires: left at
/// "preferred", 1Password signs without it while unlocked, and the machine refuses.
@MainActor
struct PlatformPasskeyTests {
    let platform = ASAuthorizationPlatformPublicKeyCredentialProvider(relyingPartyIdentifier: PlatformAuthenticator.relyingParty)
    let keys = ASAuthorizationSecurityKeyPublicKeyCredentialProvider(relyingPartyIdentifier: PlatformAuthenticator.relyingParty)

    @Test func signingRequiresUserVerification() {
        let request = PlatformAuthenticator.verified(platform.createCredentialAssertionRequest(challenge: Data([1])))
        #expect(request.userVerificationPreference == .required)
        let key = PlatformAuthenticator.verified(keys.createCredentialAssertionRequest(challenge: Data([1])))
        #expect(key.userVerificationPreference == .required)
    }

    @Test func creatingRequiresUserVerification() {
        let request = PlatformAuthenticator.verified(
            platform.createCredentialRegistrationRequest(challenge: Data([1]), name: "Tomas", userID: Data([7])))
        #expect(request.userVerificationPreference == .required)
        let key = PlatformAuthenticator.verified(
            keys.createCredentialRegistrationRequest(challenge: Data([1]), displayName: "Tomas", name: "Tomas", userID: Data([7])))
        #expect(key.userVerificationPreference == .required)
    }
}
