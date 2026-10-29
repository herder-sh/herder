import AuthenticationServices
import Foundation
import Herder
@testable import HerderKit
import Testing

/// An authenticator that signs without Face ID, or fails as told.
@MainActor final class FakeAuthenticator: Authenticator {
    var failure: Error?
    private(set) var signed: [Data] = []
    private(set) var registered: [String] = []

    func register(name: String, userId: Data, challenge: Data) async throws -> PasskeyRegistration {
        if let failure { throw failure }
        registered.append(name)
        return PasskeyRegistration(credentialId: Data([1]), attestation: Data([2]), clientData: Data([3]))
    }

    func assert(challenge: Data) async throws -> PasskeyAssertion {
        if let failure { throw failure }
        signed.append(challenge)
        return PasskeyAssertion(credentialId: Data([1]), authenticatorData: Data([4]), signature: Data([5]),
                                clientData: Data([6]))
    }
}

@MainActor
struct ApprovalTests {
    static let fingerprint = "ab129f3c" + String(repeating: "0", count: 52) + "e07d"
    static let machine = ApprovalRequest(kind: .machine, name: "new-server", code: "K7Q4PD", fingerprint: fingerprint,
                                         hosts: ["10.0.4.17:7447"], os: "Linux", version: "0.17.0")

    @Test func aMachinesLinkSaysWhereItListens() throws {
        let url = try #require(URL(string: "herder://own?host=10.0.4.17%3A7447&host=new-server.local%3A7447&fp=\(Self.fingerprint)&code=K7Q4PD"))
        #expect(AppLink(url: url) == .approve(ApprovalRequest(
            kind: .machine, name: "10.0.4.17", code: "K7Q4PD", fingerprint: Self.fingerprint,
            hosts: ["10.0.4.17:7447", "new-server.local:7447"])))
        #expect(Self.machine.about == "Linux · 10.0.4.17 · herder 0.17.0")
    }

    @Test func aTerminalsLinkAsksToSignIn() throws {
        let url = try #require(URL(string: "herder://login?host=%5Bfe80%3A%3A1%5D%3A7447&fp=\(Self.fingerprint)&code=K7Q-4PD"))
        guard case .approve(let request) = AppLink(url: url) else { Issue.record("not an approval"); return }
        #expect(request.kind == .login)
        #expect(request.name == "fe80::1")
        #expect(request.about == "fe80::1")
        #expect(request.spokenCode == "K7Q · 4PD")
    }

    @Test func aLinkWithoutItsCodeOpensNothing() throws {
        let url = try #require(URL(string: "herder://own?host=10.0.4.17%3A7447&fp=\(Self.fingerprint)"))
        #expect(AppLink(url: url) == nil)
    }

    @Test func theCodeAndFingerprintReadAsTheTerminalPrintsThem() {
        #expect(Self.machine.spokenCode == "K7Q · 4PD")
        #expect(ApprovalRequest.halves("k7q4p") == "K7Q · 4P")
        #expect(Self.machine.shortFingerprint == "ab12 9f3c … e07d")
    }

    @Test func approvingSignsTheChallengeThenSendsIt() async {
        let authenticator = FakeAuthenticator()
        var sent: [PasskeyAssertion] = []
        let approval = Approval(Self.machine, authenticator: authenticator, challenge: { Data([9, 9]) }) { sent.append($0) }
        await approval.approve()
        #expect(authenticator.signed == [Data([9, 9])])
        #expect(sent.count == 1)
        #expect(approval.state == .approved)
    }

    @Test func closingFaceIDAsksAgainButARefusalSaysWhy() async {
        let authenticator = FakeAuthenticator()
        authenticator.failure = ASAuthorizationError(.canceled)
        let approval = Approval(Self.machine, authenticator: authenticator, challenge: { Data() }) { _ in }
        await approval.approve()
        #expect(approval.state == .asking)
        authenticator.failure = nil
        let refused = Approval(Self.machine, authenticator: authenticator, challenge: { Data() }) { _ in
            throw HerderError.Local(detail: "the code expired")
        }
        await refused.approve()
        #expect(refused.state == .failed("the code expired"))
    }
}

@MainActor
struct ClaimTests {
    @Test func rowsSayWhatChanges() {
        let plan = ClaimPlan.demo
        #expect(plan.headline == "Create your herder passkey, then claim 5 machines")
        #expect(plan.detail == "Stored in iCloud Keychain; Face ID once for all five.")
        #expect(plan.machines.map(\.change) == [
            "tomas (owner) → you", "tomas (owner) → you", "tomas (owner) → you",
            "tomas, owner (owner) → you · 3 machine rows move to Machines",
            "tomas (owner) → you · draugelis moves to Machines",
        ])
    }

    @Test func claimingCreatesThePasskeyThenClaimsEveryMachine() async {
        let authenticator = FakeAuthenticator()
        var answered: [String: ClaimPlan.Answer] = [:]
        let claim = Claim(.demo, authenticator: authenticator) { _, answers, _ in answered = answers }
        claim.answers["owner@herder-vault-0"] = .remove
        await claim.claim(name: "Tomas", userId: Data([1]), challenge: Data([2]))
        #expect(authenticator.registered == ["Tomas"])
        #expect(answered == ["owner@herder-vault-0": .remove])
        #expect(claim.done)
    }

    @Test func aFailedClaimLeavesEveryMachineReady() async {
        let claim = Claim(.demo, authenticator: FakeAuthenticator()) { _, _, _ in
            throw HerderError.Local(detail: "draugelis did not answer")
        }
        await claim.claim(name: "Tomas", userId: Data([1]), challenge: Data([2]))
        #expect(!claim.done)
        #expect(claim.rows.values.allSatisfy { $0 == .ready })
        #expect(claim.error == "draugelis did not answer")
    }
}
