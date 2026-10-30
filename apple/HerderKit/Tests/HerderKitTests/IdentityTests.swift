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

    @Test func aNewPasskeyIsNamedAsYourMachinesKnowYouElseNeverEmpty() {
        #expect(Fleet.ownerName("Tomas", fullName: "Tomas A") == "Tomas")
        #expect(Fleet.ownerName(nil, fullName: "Tomas A") == "Tomas A")
        // iOS has no user name.
        #expect(Fleet.ownerName(nil, fullName: "") == "Owner")
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
    /// Three machines you own: a host, and a vault that has a machine row and a leftover owner.
    static let plan = ClaimPlan(machines: [
        ClaimPlan.Machine(hostId: "box", name: "box", userId: "u-box"),
        ClaimPlan.Machine(hostId: "lab", name: "lab", userId: "u-lab"),
        ClaimPlan.Machine(hostId: "vault", name: "herder-vault-0", userId: "u-vault"),
    ], name: "Tomas")

    static let key = PasskeyKey(credentialId: "AQ", publicKey: "pk")

    /// The machines, as `claim_users`, `list_people`, `passkey_challenge` and `decide_leftover`
    /// answer, with what each was sent.
    @MainActor final class Machines {
        var sent: [(HostId, CommandBody)] = []
        var refusing: Set<HostId> = []

        func send(_ hostId: HostId, _ command: CommandBody) async throws -> CommandResult {
            sent.append((hostId, command))
            if refusing.contains(hostId) { throw HerderError.Local(detail: "not an owner") }
            switch command {
            case .passkeyChallenge:
                return .passkeyChallenge(
                    challenge: PasskeyChallenge(rpId: "herder.sh", challenge: "CQk", expiresAt: "2026-10-29T12:01:00Z"),
                    registration: PasskeyCreation(rpId: "herder.sh", challenge: "CQk", personId: "p-\(hostId)",
                                                  userHandle: "AQ", expiresAt: "2026-10-29T12:01:00Z"))
            case .claimUsers(let person, _):
                let personId = switch person.passkey {
                case .register: "p-\(hostId)"
                case .existing(let personId, _, _): personId
                }
                guard hostId == "vault" else { return .claimed(personId: personId, hosts: [], leftovers: []) }
                let device = UserDevice(deviceId: "d", client: "herder-cli/0.6.0", pairedAt: "2026-10-09T09:00:00Z",
                                        lastSeen: nil, online: true, hostOnly: false)
                return .claimed(personId: personId, hosts: ["trash-can-01"],
                                leftovers: [UserInfo(userId: "u-owner", name: "owner", role: .owner, devices: [device])])
            case .listPeople:
                return .people(people: [PersonInfo(personId: "p-\(hostId)", name: "Tomas", role: .owner,
                                                   passkeys: [ClaimTests.key], devices: [])])
            case .decideLeftover:
                return .applied
            default:
                throw HerderError.Local(detail: "unexpected \(command)")
            }
        }

        /// What `claim_users` carried, by machine: the passkey made or reused, and `also`.
        var claims: [String] {
            sent.compactMap { hostId, command in
                guard case .claimUsers(let person, let also) = command else { return nil }
                let passkey = switch person.passkey {
                case .register: "new"
                case .existing(let personId, _, _): personId
                }
                return "\(hostId) \(passkey) also \(also.joined(separator: ","))"
            }
        }
    }

    @Test func rowsSayWhatChanges() {
        let plan = ClaimPlan.demo
        #expect(plan.headline == "Create your herder passkey, then claim 5 machines")
        #expect(plan.detail == "Stored in iCloud Keychain; Face ID once for all five.")
        #expect(plan.machines.map(\.change) == [
            "tomas (owner) → you", "tomas (owner) → you", "tomas (owner) → you",
            "tomas, owner (owner) → you · 3 machine rows move to Machines",
            "tomas (owner) → you · draugelis moves to Machines",
        ])
        #expect(Self.plan.machines[0].change == "your user (owner) → you")
    }

    @Test func onlyConnectedMachinesYouOwnWithoutAPersonAreUnclaimed() {
        func owned(_ id: HostId, role: Role = .owner, personId: PersonId? = nil,
                   connection: ConnectionState = .connected) -> Machine {
            var machine = machine(id, name: id, sessions: [], role: role, personId: personId, connection: connection)
            machine.userId = "u-\(id)"
            return machine
        }
        let fleet = [owned("box"), owned("claimed", personId: "p"), owned("shared", role: .member),
                     owned("away", connection: .connecting), machine("new", name: "new", sessions: [])]
        #expect(ClaimPlan.unclaimed(fleet) == [ClaimPlan.Machine(hostId: "box", name: "box", userId: "u-box")])
    }

    @Test func claimingMakesThePasskeyOnceAndEachMachineTakesTheOthersUsers() async {
        let authenticator = FakeAuthenticator()
        let machines = Machines()
        var refreshed: [HostId] = []
        let claim = Claim(Self.plan, authenticator: authenticator, send: machines.send) { refreshed.append($0) }
        await claim.claim()
        #expect(authenticator.registered == ["Tomas"])
        // One Face ID for every machine after the first: their challenges, one after the other.
        #expect(authenticator.signed == [Data([9, 9, 9, 9])])
        #expect(refreshed == ["box", "lab", "vault"], "each machine's hello then names you")
        #expect(machines.claims == ["box new also u-lab,u-vault", "lab p-box also u-box,u-vault",
                                    "vault p-box also u-box,u-lab"])
        #expect(claim.claimed)
        #expect(claim.machines[2].change == "your user (owner) → you · trash-can-01 moves to Machines")
        #expect(claim.questions.map(\.id) == ["u-owner@vault"])
        #expect(claim.questions[0].reason.hasPrefix("An owner from"))
        #expect(claim.questions[0].reason.hasSuffix("whose device is online. Is this you?"))
        #expect(!claim.done, "the leftover owner waits for a decision")
    }

    @Test func aPasskeyYourMachinesKnowIsReusedNotMadeAnew() async {
        var plan = Self.plan
        plan.known = [PersonInfo(personId: "p-me", name: "Tomas", role: .owner, passkeys: [Self.key], devices: [])]
        #expect(plan.headline == "Claim 3 machines with your herder passkey")
        let authenticator = FakeAuthenticator()
        let machines = Machines()
        let claim = Claim(plan, authenticator: authenticator, send: machines.send)
        await claim.claim()
        #expect(authenticator.registered.isEmpty)
        #expect(authenticator.signed.count == 1, "Face ID once for all three")
        #expect(machines.claims.map { $0.split(separator: " ")[1] } == ["p-me", "p-me", "p-me"])
        #expect(claim.claimed)
    }

    @Test func anotherDevicesPersonIsReusedByThePasskeyThatSigns() {
        let sam = PersonInfo(personId: "p-sam", name: "Sam", role: .owner, passkeys: [PasskeyKey(credentialId: "Ag", publicKey: "pk")],
                             devices: [])
        let tomas = PersonInfo(personId: "p-me", name: "Tomas", role: .owner, passkeys: [Self.key], devices: [])
        let member = PersonInfo(personId: "p-kim", name: "Kim", role: .member, passkeys: [Self.key], devices: [])
        let people: [HostId: [PersonInfo]] = ["box": [tomas, member], "lab": [sam, tomas]]
        // No machine says this device is anyone yet: every owner with a passkey may be you.
        #expect(Fleet.yours(nil, in: people).map(\.personId) == ["p-me", "p-sam"])
        #expect(Fleet.yours("p-sam", in: people).map(\.personId) == ["p-sam"])
    }

    @Test func theOwnerWhosePasskeySignsIsYou() async {
        var plan = Self.plan
        plan.known = [PersonInfo(personId: "p-sam", name: "Sam", role: .owner,
                                 passkeys: [PasskeyKey(credentialId: "Ag", publicKey: "pk")], devices: []),
                      PersonInfo(personId: "p-me", name: "Tomas", role: .owner, passkeys: [Self.key], devices: [])]
        let machines = Machines()
        let claim = Claim(plan, authenticator: FakeAuthenticator(), send: machines.send)
        await claim.claim()
        #expect(machines.claims.map { $0.split(separator: " ")[1] } == ["p-me", "p-me", "p-me"])
        #expect(claim.claimed)
    }

    @Test func aNameNobodyKnowsIsAskedForNotMobileUser() {
        #if os(iOS)
        #expect(Fleet.deviceUserName.isEmpty)
        #endif
        let claim = Claim(ClaimPlan(machines: Self.plan.machines, name: ""), authenticator: FakeAuthenticator(),
                          send: Machines().send)
        #expect(claim.name.isEmpty, "the sheet asks for it")
    }

    @Test func decisionsGoToTheLeftoversMachineAndSomeoneElseStays() async {
        let machines = Machines()
        let claim = Claim(Self.plan, authenticator: FakeAuthenticator(), send: machines.send)
        await claim.claim()
        claim.answers["u-owner@vault"] = .remove
        await claim.decide()
        let decided = machines.sent.compactMap { hostId, command -> String? in
            guard case .decideLeftover(let userId, let decision, _) = command else { return nil }
            return "\(hostId) \(userId) \(decision)"
        }
        #expect(decided == ["vault u-owner remove"])
        #expect(claim.done)

        let other = Machines()
        let kept = Claim(Self.plan, authenticator: FakeAuthenticator(), send: other.send)
        await kept.claim()
        kept.answers["u-owner@vault"] = .someoneElse
        await kept.decide()
        #expect(!other.sent.contains { if case .decideLeftover = $0.1 { true } else { false } })
        #expect(kept.done)
    }

    @Test func aMachineThatRefusesIsMarkedAndTheOthersAreClaimed() async {
        let machines = Machines()
        machines.refusing = ["lab"]
        let claim = Claim(Self.plan, authenticator: FakeAuthenticator(), send: machines.send)
        await claim.claim()
        #expect(claim.rows == ["box": .claimed, "lab": .failed, "vault": .claimed])
        #expect(claim.error == "lab: not an owner")
        #expect(!claim.claimed)
    }

    @Test func closingFaceIDLeavesEveryMachineReady() async {
        let authenticator = FakeAuthenticator()
        authenticator.failure = ASAuthorizationError(.canceled)
        let claim = Claim(Self.plan, authenticator: authenticator, send: Machines().send)
        await claim.claim()
        #expect(claim.rows.values.allSatisfy { $0 == .ready })
        #expect(claim.error == nil)
        #expect(!claim.working)
    }
}
