import CryptoKit
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
        return PasskeyRegistration(id: "AQ", rawId: "AQ",
                                   response: RegistrationResponse(clientDataJson: "Aw", attestationObject: "Ag"),
                                   kind: "public-key")
    }

    func assert(challenge: Data) async throws -> PasskeyAssertion {
        if let failure { throw failure }
        signed.append(challenge)
        return PasskeyAssertion(id: "AQ", rawId: "AQ",
                                response: AssertionResponse(clientDataJson: "Bg", authenticatorData: "BA",
                                                            signature: "BQ", userHandle: "AQ"),
                                kind: "public-key")
    }
}

@MainActor
struct ApprovalTests {
    /// A challenge over the bytes 9, 9.
    static let challenge = PasskeyChallenge(rpId: "herder.sh", challenge: "CQk", expiresAt: "2026-10-29T12:01:00Z")
    static let fingerprint = "ab129f3c" + String(repeating: "0", count: 52) + "e07d"
    static let machine = ApprovalRequest(kind: .machine, name: "new-server", code: "K7Q4PD", fingerprint: fingerprint,
                                         hosts: ["10.0.4.17:7447"])

    @Test func aMachinesLinkNamesIt() throws {
        let url = try #require(URL(string: "herder://own?host=10.0.4.17%3A7447&host=new-server.local%3A7447&fp=\(Self.fingerprint)&code=K7Q4PD&name=new-server"))
        #expect(AppLink(url: url) == .approve(ApprovalRequest(
            kind: .machine, name: "new-server", code: "K7Q4PD", fingerprint: Self.fingerprint,
            hosts: ["10.0.4.17:7447", "new-server.local:7447"])))
        #expect(Self.machine.about == "10.0.4.17")
    }

    @Test func anUnnamedMachineGoesByItsAddress() throws {
        let url = try #require(URL(string: "herder://own?host=10.0.4.17%3A7447&fp=\(Self.fingerprint)&code=K7Q4PD"))
        guard case .approve(let request) = AppLink(url: url) else { Issue.record("not an approval"); return }
        #expect(request.name == "10.0.4.17")
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

    @Test func bytesRoundTripAsBase64URL() {
        let bytes = Data([0xFB, 0xFF, 0x01])
        #expect(bytes.base64URLEncodedString() == "-_8B")
        #expect(Data(base64URL: "-_8B") == bytes)
        #expect(Data(base64URL: "CQk") == Data([9, 9]))
    }

    @Test func theCodeAndFingerprintReadAsTheTerminalPrintsThem() {
        #expect(Self.machine.spokenCode == "K7Q · 4PD")
        #expect(ApprovalRequest.halves("k7q4p") == "K7Q · 4P")
        #expect(Self.machine.shortFingerprint == "ab12 9f3c … e07d")
    }

    @Test func approvingRunsWithThePasskeyAndKeepsTheNewMachine() async {
        let authenticator = FakeAuthenticator()
        let approval = Approval(Self.machine, authenticator: authenticator) { passkey in
            _ = try await passkey.sign(Self.challenge)
            return "new-server-host"
        }
        await approval.approve()
        #expect(authenticator.signed == [Data([9, 9])])
        #expect(approval.state == .approved)
        #expect(approval.machine == "new-server-host")
    }

    @Test func aNewMachinesOwnerIsYouWithYourPasskeyWhenAMachineKnowsIt() {
        let key = PasskeyKey(credentialId: "AQ", publicKey: "pk")
        let people: [HostId: [PersonInfo]] = [
            "devbox": [PersonInfo(personId: "me", name: "Tomas", role: .owner, passkeys: [], devices: [])],
            "studio": [PersonInfo(personId: "me", name: "Tomas", role: .owner, passkeys: [key], devices: []),
                       PersonInfo(personId: "sam", name: "Sam", role: .member, passkeys: [key], devices: [])],
        ]
        #expect(Fleet.keyed("me", in: people)?.passkeys == [key])
        #expect(Fleet.keyed("kim", in: people) == nil)
        #expect(Fleet.keyed(nil, in: people) == nil)
    }

    @Test func closingFaceIDAsksAgainButARefusalSaysWhy() async {
        let authenticator = FakeAuthenticator()
        authenticator.failure = ASAuthorizationError(.canceled)
        let approval = Approval(Self.machine, authenticator: authenticator) { try await $0.sign(Self.challenge); return nil }
        await approval.approve()
        #expect(approval.state == .asking)
        authenticator.failure = nil
        let refused = Approval(Self.machine, authenticator: authenticator) { _ in
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
        #expect(plan.detail == "Stored in iCloud Keychain; Face ID once for each machine.")
        #expect(plan.machines.map(\.change) == [
            "tomas (owner) → you", "tomas (owner) → you", "tomas (owner) → you",
            "tomas, owner (owner) → you · 3 machine rows move to Machines",
            "tomas (owner) → you · draugelis moves to Machines",
        ])
    }

    static func device(_ client: String, online: Bool = true, hostOnly: Bool = false) -> UserDevice {
        UserDevice(deviceId: "d-\(client)", client: client, pairedAt: "2026-10-09T10:00:00Z", lastSeen: nil,
                   online: online, hostOnly: hostOnly)
    }

    @Test func aMachinesUsersSayWhatMoves() throws {
        let users = [
            UserInfo(userId: "u1", name: "tomas", role: .owner, devices: [Self.device("herder-macos/0.17.0")]),
            UserInfo(userId: "u2", name: "owner", role: .owner, devices: [Self.device("herder/0.17.0")]),
            UserInfo(userId: "u3", name: "trash-can-01", role: .member, devices: [Self.device("herder/0.17.0", hostOnly: true)]),
            UserInfo(userId: "u4", name: "sam", role: .member, devices: [Self.device("herder-ios/0.17.0", online: false)]),
        ]
        let (machine, questions) = try #require(ClaimPlan.machine("vault", name: "herder-vault-0", users: users,
                                                                  thisClient: "herder-macos"))
        #expect(machine.me == "u1")
        #expect(machine.change == "tomas (owner) → you · trash-can-01 moves to Machines")
        #expect(questions.map(\.user) == ["owner", "sam"])
        #expect(questions.map(\.suggested) == [.me, .someoneElse])
        let oct9 = Timestamp.date("2026-10-09T10:00:00Z")!.formatted(.dateTime.month(.abbreviated).day())
        #expect(questions[0].reason == "A user from \(oct9) whose CLI device is online. Is this you?")
        #expect(questions[1].reason == "A user from \(oct9) whose iPhone device is offline. Is this you?")
        // A machine where none of the users is this device's is not yours to claim.
        #expect(ClaimPlan.machine("x", name: "x", users: Array(users.dropFirst()), thisClient: "herder-macos") == nil)
    }

    /// Machines that answer as the daemon does: a challenge with what to make a passkey from,
    /// the claim, and the people with the new passkey.
    @MainActor final class Machines {
        var sent: [(HostId, CommandBody)] = []
        var failing: Set<HostId> = []

        func send(_ hostId: HostId, _ command: CommandBody) throws -> CommandResult {
            sent.append((hostId, command))
            if failing.contains(hostId) { throw HerderError.Local(detail: "\(hostId) did not answer") }
            switch command {
            case .passkeyChallenge:
                return .passkeyChallenge(challenge: ApprovalTests.challenge, registration: PasskeyCreation(
                    rpId: "herder.sh", challenge: "Ag", personId: "p1", userHandle: "AQ", expiresAt: "2026-10-29T12:01:00Z"))
            case .claimUsers:
                return .claimed(personId: "p1", hosts: [],
                                leftovers: [UserInfo(userId: "u-owner", name: "owner", role: .owner, devices: [])])
            case .listPeople:
                return .people(people: [PersonInfo(personId: "p1", name: "Tomas", role: .owner,
                                                   passkeys: [PasskeyKey(credentialId: "AQ", publicKey: "pk")], devices: [])])
            default:
                return .applied
            }
        }

        func claims(_ hostId: HostId) -> [CommandBody] {
            sent.filter { $0.0 == hostId }.map(\.1).filter { if case .claimUsers = $0 { true } else { false } }
        }
    }

    @Test func thePasskeyIsMadeOnceThenEveryOtherMachineIsClaimedWithIt() async throws {
        let authenticator = FakeAuthenticator()
        let machines = Machines()
        let claim = Claim(.demo, name: "Tomas", authenticator: authenticator) { try machines.send($0, $1) }
        await claim.claim()
        #expect(claim.done)
        #expect(authenticator.registered == ["Tomas"])
        guard case .claimUsers(let first, let also) = try #require(machines.claims("mac").first),
              case .register = first.passkey else { Issue.record("the first machine made no passkey"); return }
        #expect(also == ["u-trash", "u-draugelis", "u-vault", "u-dvault"])
        for host in ["trash", "draugelis", "vault", "dvault"] {
            guard case .claimUsers(let person, _) = try #require(machines.claims(HostId(host)).first),
                  case .existing(let personId, let key, _) = person.passkey
            else { Issue.record("\(host) was not claimed with the passkey"); continue }
            #expect(personId == "p1")
            #expect(key.credentialId == "AQ")
        }
        // "owner" on the vault was taken to be you; no other machine has a question.
        let decided = machines.sent.compactMap { host, command -> HostId? in
            if case .decideLeftover(let userId, .itsMe, _) = command, userId == "u-owner" { host } else { nil }
        }
        #expect(decided == ["vault"])
    }

    @Test func someoneElseIsLeftAlone() async {
        let machines = Machines()
        let claim = Claim(.demo, name: "Tomas", authenticator: FakeAuthenticator()) { try machines.send($0, $1) }
        claim.answers[ClaimPlan.demo.questions[0].id] = .someoneElse
        await claim.claim()
        #expect(!machines.sent.contains { if case .decideLeftover = $0.1 { true } else { false } })
    }

    @Test func aMachineThatFailsSaysWhyAndTheRestAreClaimed() async {
        let machines = Machines()
        machines.failing = ["draugelis"]
        let claim = Claim(.demo, name: "Tomas", authenticator: FakeAuthenticator()) { try machines.send($0, $1) }
        await claim.claim()
        #expect(claim.rows["draugelis"] == .failed("draugelis did not answer"))
        #expect(claim.rows.filter { $0.value == .claimed }.count == 4)
        machines.failing = []
        await claim.claim()
        #expect(claim.done)
    }

    @Test func closingFaceIDStopsAndLeavesTheRestReady() async {
        let authenticator = FakeAuthenticator()
        authenticator.failure = ASAuthorizationError(.canceled)
        let claim = Claim(.demo, name: "Tomas", authenticator: authenticator) { try Machines().send($0, $1) }
        await claim.claim()
        #expect(claim.rows.values.allSatisfy { $0 == .ready })
    }
}

@MainActor
struct SoftwarePasskeyTests {
    @Test func itsAssertionVerifiesWithTheKeyItRegistered() async throws {
        let passkey = SoftwareAuthenticator()
        let registration = try await passkey.register(name: "Tomas", userId: Data([1]), challenge: Data([2]))
        let attestation = try #require(Data(base64URL: registration.response.attestationObject))
        // The COSE key ends the attestation: x after its 10th byte, y after its 45th.
        let cose = attestation.suffix(77)
        #expect(Array(cose.prefix(8)) == [0xA5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21])
        let key = try P256.Signing.PublicKey(x963Representation: Data([0x04]) + cose.dropFirst(10).prefix(32)
                                                 + cose.dropFirst(45).prefix(32))
        let assertion = try await passkey.assert(challenge: Data([9, 9]))
        let clientData = try #require(Data(base64URL: assertion.response.clientDataJson))
        let authData = try #require(Data(base64URL: assertion.response.authenticatorData))
        let signature = try P256.Signing.ECDSASignature(
            derRepresentation: try #require(Data(base64URL: assertion.response.signature)))
        #expect(key.isValidSignature(signature, for: authData + Data(SHA256.hash(data: clientData))))
        let client = try #require(try JSONSerialization.jsonObject(with: clientData) as? [String: Any])
        #expect(client["type"] as? String == "webauthn.get")
        #expect(client["challenge"] as? String == "CQk")
        #expect(client["origin"] as? String == "https://herder.sh")
        #expect(assertion.id == registration.id)
    }
}
