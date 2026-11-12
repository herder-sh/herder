import AuthenticationServices
import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct YouTests {
    nonisolated static let apple = "fbfc3007-154e-4ecc-8c0b-6e020557d7bd"
    nonisolated static let onePassword = "bada5566-a7aa-401f-bd96-45619a55120d"

    static func key(_ id: String, aaguid: String? = nil, added: String? = nil) -> PasskeyKey {
        PasskeyKey(credentialId: id, publicKey: "{}", aaguid: aaguid, addedAt: added)
    }

    /// Tomas on three machines: devbox and nas know both his passkeys, laptop only the one in
    /// Apple Passwords, and laptop lists the 1Password one's AAGUID where devbox does not.
    static func directory() -> (Directory, [HostId: [PersonInfo]]) {
        let older = key("apple", aaguid: apple, added: "2026-10-03T09:00:00Z")
        let newer = key("1p", added: "2026-11-11T09:00:00Z")
        let people: [HostId: [PersonInfo]] = [
            "devbox": [PersonInfo(personId: "p-tomas", name: "Tomas", role: .owner, passkeys: [newer, older], devices: []),
                       person("p-sam", "Sam", .member)],
            "nas": [PersonInfo(personId: "p-tomas", name: "Tomas", role: .owner,
                               passkeys: [key("1p", aaguid: onePassword, added: "2026-11-12T09:00:00Z"), older], devices: [])],
            "laptop": [PersonInfo(personId: "p-tomas", name: "Tomas", role: .member, passkeys: [older], devices: [])],
            "kim": [person("p-kim", "Kim", .owner)],
        ]
        let columns = ["devbox", "nas", "laptop", "kim"].map { Directory.Column(id: $0, name: $0) }
        let directory = Directory.merging(people, columns: columns, me: "p-tomas", thisClient: "herder-macos/")
        return (directory, people)
    }

    @Test func passkeysAreMergedAcrossTheMachinesThatKnowThem() throws {
        let (directory, people) = Self.directory()
        let you = You(directory, people: people, thisDevice: "apple")
        #expect(you.columns.map(\.id) == ["devbox", "nas", "laptop"], "a machine that does not know you is not yours")
        #expect(you.passkeys.map(\.id) == ["apple", "1p"], "oldest first")
        let apple = you.passkeys[0], onePassword = you.passkeys[1]
        #expect(apple.thisDevice && !onePassword.thisDevice)
        #expect(apple.store == "Apple Passwords")
        #expect(onePassword.store == "1Password", "any machine that kept the AAGUID names it")
        #expect(onePassword.addedAt == Timestamp.date("2026-11-11T09:00:00Z"), "the first machine it was added to")
        #expect(apple.machines.map(\.id) == ["devbox", "nas", "laptop"])
        #expect(onePassword.machines.map(\.id) == ["devbox", "nas"])
        #expect(apple.only.map(\.id) == ["laptop"])
        #expect(you.reach(apple) == "on 3 machines")
        #expect(you.reach(onePassword) == "on 2 of 3 machines")
        #expect(you.roles == "Owner of 2 machines · member of 1")
    }

    @Test func youAreCoveredWithTwoPasskeys() {
        let (directory, people) = Self.directory()
        var you = You(directory, people: people, thisDevice: nil)
        #expect(you.covered)
        #expect(you.coverage == "2 passkeys in 2 places, and 0 devices.")
        you.passkeys.removeLast()
        #expect(!you.covered)
        #expect(you.coverage == "1 passkey and 0 devices.")
        #expect(You.demo(now: .now).coverage == "2 passkeys in 2 places, and 3 devices.")
    }

    @Test func removingSaysWhatStillWorks() {
        let (directory, people) = Self.directory()
        let you = You(directory, people: people, thisDevice: nil)
        #expect(you.afterRemoving(you.passkeys[1])
            == "You’ll still sign in with your Apple Passwords passkey. Every machine forgets this one.")
        #expect(you.afterRemoving(you.passkeys[0])
            == "You’ll still sign in with your 1Password passkey. laptop knows you by this one alone, so it keeps it until you add another there.")
        var alone = you
        alone.passkeys = [you.passkeys[0]]
        #expect(alone.afterRemoving(alone.passkeys[0]) == "This is your only passkey, so it can’t be removed. Add another first.")
    }

    @Test func devicesSayHowManyMachinesTheyAreOn() {
        let you = You.demo(now: .now)
        #expect(you.devices.map(YouSections.name) == ["iPhone", "Mac", "Terminal on trash-can-01"])
        #expect(you.reach(you.devices[0]) == "On 5 of 5 machines")
        #expect(you.roles == "Owner of 5 machines")
        #expect(You.short("7Q2FbN8sHc1JdK9F9") == "7Q2F…K9F9")
    }

    @Test func storesAreNamedByAAGUIDInAnyCase() {
        #expect(PasskeyStore.name(Self.onePassword.uppercased()) == "1Password")
        #expect(PasskeyStore.name("00000000-0000-0000-0000-000000000000") == nil)
        #expect(PasskeyStore.name(nil) == nil)
    }

    @Test func removingSkipsTheMachinesThatKnowYouByItAlone() async throws {
        let (directory, people) = Self.directory()
        let you = You(directory, people: people, thisDevice: nil)
        var sent: [(HostId, CommandBody)] = []
        try await You.remove(you.passkeys[0], send: { hostId, command in
            if case .passkeyChallenge = command { return Fake.challenge(hostId) }
            sent.append((hostId, command))
            return .applied
        }, sign: Fake.sign)
        #expect(sent.map(\.0) == ["devbox", "nas"])
        #expect(sent.allSatisfy {
            if case .removePasskey(let id, let assertion) = $0.1 { id == "apple" && assertion.response.clientDataJson == "c-\($0.0)" } else { false }
        })
    }
}

/// A fleet that takes passkeys: each machine's challenge names it.
@MainActor
enum Fake {
    static func challenge(_ hostId: HostId) -> CommandResult {
        .passkeyChallenge(challenge: PasskeyChallenge(rpId: "herder.sh", challenge: "c-\(hostId)", expiresAt: "2026-11-12T12:01:00Z"),
                          registration: PasskeyCreation(rpId: "herder.sh", challenge: "r-\(hostId)", personId: "p-tomas",
                                                        userHandle: "dXNlcg", expiresAt: "2026-11-12T12:01:00Z"))
    }

    /// Your passkey, signing as `cred-you`; the challenge it signed shows as its client data.
    static let sign: PasskeySigner = { challenge in Fake.assertion("cred-you", over: challenge.challenge) }

    static func assertion(_ id: String, over challenge: String) -> PasskeyAssertion {
        PasskeyAssertion(id: id, rawId: id, response: AssertionResponse(
            clientDataJson: challenge, authenticatorData: "", signature: "", userHandle: nil), kind: "public-key")
    }

    static let registration = PasskeyRegistration(
        id: "new", rawId: "new", response: RegistrationResponse(clientDataJson: "", attestationObject: ""), kind: "public-key")

    static let newKey = PasskeyKey(credentialId: "new", publicKey: "{}", aaguid: YouTests.onePassword, addedAt: "2026-11-12T12:00:00Z")
}

@MainActor
struct PasskeyAddingTests {
    let machines = ["devbox", "nas", "laptop", "vault"].map { Directory.Column(id: $0, name: $0) }

    /// Adding to `machines`, where `fail` says how a machine answers `add_passkey`.
    private func adding(fail: @escaping (HostId) -> Error? = { _ in nil },
                        create: @escaping PasskeyAdding.Create = { _, _ in Fake.registration })
    -> (PasskeyAdding, () -> [(HostId, CommandBody)]) {
        var sent: [(HostId, CommandBody)] = []
        let adding = PasskeyAdding(
            personId: "p-tomas", machines: machines,
            send: { hostId, command in
                switch command {
                case .passkeyChallenge: return Fake.challenge(hostId)
                case .listPeople:
                    return .people(people: [PersonInfo(personId: "p-tomas", name: "Tomas", role: .owner,
                                                       passkeys: [Fake.newKey], devices: [])])
                default:
                    sent.append((hostId, command))
                    if let error = fail(hostId) { throw error }
                    return .applied
                }
            },
            sign: Fake.sign, create: create,
            signNew: { challenge, id, _ in Fake.assertion(id, over: challenge.challenge) })
        return (adding, { sent })
    }

    @Test func theFirstMachineMakesItAndTheRestLearnItFromTheFirstsListing() async {
        let (adding, sent) = adding(fail: { $0 == "vault" ? HerderError.Rejected(info: ErrorInfo(code: .conflict, message: "has it")) : nil })
        await adding.run()
        #expect(adding.rows.map(\.state) == [.added, .added, .added, .added], "a machine that has it already counts as added")
        #expect(adding.key == Fake.newKey)
        let commands = sent()
        #expect(commands.map(\.0) == ["devbox", "nas", "laptop", "vault"])
        #expect(commands[0].1 == .addPasskey(passkey: .register(registration: Fake.registration),
                                             assertion: Fake.assertion("cred-you", over: "c-devbox")))
        #expect(commands[1].1 == .addPasskey(
            passkey: .existing(personId: "p-tomas", key: Fake.newKey, assertion: Fake.assertion("new", over: "c-nas")),
            assertion: Fake.assertion("cred-you", over: "c-nas")))
        #expect(adding.summary(biometry: "Face ID") == "Added to all 4.")
    }

    @Test func aMachineThatFailsIsLeftToTryAgain() async {
        var down: Set<HostId> = ["nas"]
        let (adding, sent) = adding(fail: { down.contains($0) ? HerderError.Unreachable(detail: "timed out") : nil })
        await adding.run()
        #expect(adding.rows.map(\.state.pending) == [false, true, false, false])
        #expect(adding.summary(biometry: "Face ID") == "Added to 3 of 4. Try again once the rest can be reached.")
        down = []
        await adding.run()
        #expect(adding.rows.allSatisfy { $0.state == .added })
        #expect(sent().map(\.0) == ["devbox", "nas", "laptop", "vault", "nas"], "only what was left is tried again")
    }

    @Test func closingTheCreateSheetStopsWithNothingSent() async {
        let (adding, sent) = adding(create: { _, _ in throw ASAuthorizationError(.canceled) })
        await adding.run()
        #expect(adding.rows.allSatisfy { $0.state == .waiting })
        #expect(sent().isEmpty)
        #expect(!adding.running)
    }

    @Test func aFirstMachineThatRefusesTheNewPasskeyStopsTheRest() async {
        let (adding, sent) = adding(fail: { $0 == "devbox" ? HerderError.Rejected(info: ErrorInfo(code: .forbidden, message: "bad assertion")) : nil })
        await adding.run()
        #expect(adding.rows.map(\.state.pending) == [true, true, true, true])
        #expect(adding.key == nil)
        // Trying the next machine would make yet another passkey.
        #expect(sent().map(\.0) == ["devbox"])
    }
}
