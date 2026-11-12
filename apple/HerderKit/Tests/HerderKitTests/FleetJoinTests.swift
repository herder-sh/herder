import AuthenticationServices
import CryptoKit
import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor struct FleetJoinTests {
    nonisolated static func machine(_ n: Int) -> FleetMachine {
        FleetMachine(hosts: ["10.0.4.\(n):7447"], fingerprint: String(repeating: String(format: "%02x", n), count: 32))
    }

    nonisolated static let person = FleetPerson(name: "alice", passkeys: ["Y3JlZC0x"])
    nonisolated static let link = FleetLink(person: person, machines: (1...5).map(machine))

    @Test func skipsTheMachinesThisDeviceHasAlready() async {
        var asked: [String] = []
        let join = FleetJoin(Self.link, known: { $0 == Self.machine(2).fingerprint ? "devbox" : nil }) { machine in
            asked.append(machine.fingerprint)
            return "host-\(machine.hosts[0])"
        }
        #expect(join.rows[1].state == .added("devbox"))
        #expect(join.title == "Sign in to 4 machines as you")
        await join.run()
        #expect(!asked.contains(Self.machine(2).fingerprint) && asked.count == 4)
        #expect(join.rows[1].state == .added("devbox"))
        #expect(join.rows[0].state == .signedIn("host-10.0.4.1:7447"))
        #expect(join.pending == 0)
        #expect(join.summary(biometry: "Face ID") == "Signed in to all 4.")
    }

    @Test func namesEachMachineAsTheLinkDoesElseByItsAddress() async {
        var named = Self.machine(1)
        named.name = "alice’s vault"
        var blank = Self.machine(3)
        blank.name = ""
        let link = FleetLink(person: Self.person, machines: [named, Self.machine(2), blank, Self.machine(4)])
        let join = FleetJoin(link, known: { $0 == Self.machine(4).fingerprint ? "devbox" : nil }) { machine in
            if machine.hosts[0] == "10.0.4.1:7447" { throw HerderError.Unreachable(detail: "down") }
            return "host-\(machine.hosts[0])"
        }
        #expect(join.rows.map(\.name) == ["alice’s vault", "10.0.4.2", "10.0.4.3", "devbox"])
        await join.run()
        // Signing in names it as the machine does; one it could not reach keeps the link's name.
        #expect(join.rows.map(\.name) == ["alice’s vault", "host-10.0.4.2:7447", "host-10.0.4.3:7447", "devbox"])
        #expect(FleetJoin.name(named) == "alice’s vault")
        #expect(FleetJoin.name(Self.machine(2)) == "10.0.4.2")
    }

    @Test func saysWhyEachMachineDidNotSignIn() async {
        let join = FleetJoin(Self.link, known: { _ in nil }) { machine in
            switch machine.hosts[0] {
            case "10.0.4.2:7447": throw HerderError.Unreachable(detail: "10.0.4.2:7447: timed out")
            case "10.0.4.3:7447":
                throw HerderError.Rejected(info: ErrorInfo(code: .forbidden, message: "this passkey's person is not on this daemon"))
            case "10.0.4.4:7447": throw HerderError.Rejected(info: ErrorInfo(code: .badRequest, message: "this device is paired already"))
            default: return "ok"
            }
        }
        #expect(join.summary(biometry: "Face ID") == "Face ID asks once for each machine.")
        await join.run()
        #expect(join.rows.map(\.state) == [
            .signedIn("ok"), .unreachable, .unknownPasskey, .failed("this device is paired already"), .signedIn("ok"),
        ])
        #expect(join.summary(biometry: "Face ID") == "Signed in to 2 of 5. 3 need you.")
        #expect(FleetJoin.State.unreachable.help.contains("Tailscale"))
        #expect(FleetJoin.State.unknownPasskey.help.contains("herder claim"))
    }

    @Test func closingFaceIDStopsAndTryAgainSignsInToTheRest() async {
        var cancel = true
        var asked: [String] = []
        let join = FleetJoin(Self.link, known: { _ in nil }) { machine in
            asked.append(machine.hosts[0])
            if machine.hosts[0] == "10.0.4.1:7447" { throw HerderError.Unreachable(detail: "down") }
            if machine.hosts[0] == "10.0.4.3:7447", cancel { throw ASAuthorizationError(.canceled) }
            return "ok"
        }
        await join.run()
        #expect(join.rows.map(\.state) == [.unreachable, .signedIn("ok"), .waiting, .waiting, .waiting])
        #expect(asked == ["10.0.4.1:7447", "10.0.4.2:7447", "10.0.4.3:7447"])

        cancel = false
        asked = []
        await join.run()
        // Signed in stays signed in; what failed or waited is tried again.
        #expect(asked == ["10.0.4.1:7447", "10.0.4.3:7447", "10.0.4.4:7447", "10.0.4.5:7447"])
        #expect(join.rows.map(\.state) == [.unreachable, .signedIn("ok"), .signedIn("ok"), .signedIn("ok"), .signedIn("ok")])
    }

    /// A fresh device takes the link of a device on four machines: one it gets into, one that
    /// does not know the passkey, one that is down, and one it has already. It gets in where
    /// it can, says why not elsewhere, and leaves the one it has alone.
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func signsInToTheMachinesThatKnowThePasskey() async throws {
        let mine = try FakeDaemon(name: "devbox")
        let theirs = try FakeDaemon(name: "their-box")
        let added = try FakeDaemon(name: "laptop")
        let passkey = try #require(RecordedPasskey(mine.passkey))
        let profile = temporaryProfile()
        let fleet = Fleet(client: try Client.open(configDir: profile.path, client: "phone"),
                          doneFile: profile.appendingPathComponent("done.json"))
        try await fleet.signIn(to: added)

        // Every fake daemon knows the same recorded passkey: their-box gets one nobody has,
        // which the sharing person's link lists too.
        let strangerId = Data((0..<16).map { _ in UInt8.random(in: 0...255) })
        let stranger = try #require(RecordedPasskey([
            strangerId, P256.Signing.PrivateKey().derRepresentation, Data((0..<16).map { _ in UInt8.random(in: 0...255) }),
        ].map { $0.base64URLEncodedString() }.joined(separator: ".")))
        let owner = try parseFleetLink(link: mine.link).person
        let person = FleetPerson(name: owner.name, passkeys: owner.passkeys + [strangerId.base64URLEncodedString()])
        let down = FleetMachine(hosts: ["127.0.0.1:1"], fingerprint: String(repeating: "ef", count: 32))
        let link = FleetLink(person: person, machines: [
            try parseFleetLink(link: mine.link).machines[0],
            try parseFleetLink(link: theirs.link).machines[0],
            down,
            try parseFleetLink(link: added.link).machines[0],
        ])
        let shared = try #require(pairLink(in: fleetLinkToString(link: link)))
        guard case .fleet(let parsed) = shared else { Issue.record("not a fleet link"); return }
        #expect(parsed.person == person)

        let theirFingerprint = try parseFleetLink(link: theirs.link).machines[0].fingerprint
        let signer = AllowedPasskeys()
        let join = FleetJoin(parsed, known: { fleet.machine(fingerprint: $0)?.name }) {
            signer.passkey = $0.fingerprint == theirFingerprint ? stranger : passkey
            return try await fleet.join($0, as: parsed.person, with: signer)
        }
        #expect(join.person == person)
        #expect(join.title == "Sign in to 3 machines as you")
        await join.run()
        #expect(join.rows.map(\.state) == [.signedIn("devbox"), .unknownPasskey, .unreachable, .added("laptop")])
        #expect(Set(fleet.client.machines().map(\.name)) == ["devbox", "laptop"])
        // Each machine's challenge allowed only the person's passkeys.
        let allowed = person.passkeys.compactMap { Data(base64URL: $0) }
        #expect(signer.allowed == [allowed, allowed])
    }

    /// A link that does not list this device's passkey signs in nowhere: the passkey is not
    /// offered, as Face ID offers none of the person's then.
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func signsInOnlyWithThePasskeysOfTheLinksPerson() async throws {
        let mine = try FakeDaemon(name: "devbox")
        let passkey = try #require(RecordedPasskey(mine.passkey))
        let profile = temporaryProfile()
        let fleet = Fleet(client: try Client.open(configDir: profile.path, client: "phone"),
                          doneFile: profile.appendingPathComponent("done.json"))
        let link = FleetLink(person: FleetPerson(name: "bob", passkeys: ["Ym9i"]),
                             machines: try parseFleetLink(link: mine.link).machines)
        let join = FleetJoin(link, known: { _ in nil }) { try await fleet.join($0, as: link.person, with: passkey) }
        await join.run()
        #expect(join.rows.map(\.state) == [.failed("this passkey is not one the link allows")])
        #expect(fleet.client.machines().isEmpty)
    }
}

/// Signs with `passkey`, recording the passkeys each assertion allowed.
@MainActor private final class AllowedPasskeys: Authenticator {
    var passkey: Authenticator?
    private(set) var allowed: [[Data]] = []

    func register(name: String, userId: Data, challenge: Data) async throws -> PasskeyRegistration {
        throw HerderError.Local(detail: "not in these tests")
    }

    func assert(challenge: Data, allowing credentials: [Data]) async throws -> PasskeyAssertion {
        allowed.append(credentials)
        guard let passkey else { throw HerderError.Local(detail: "no passkey") }
        return try await passkey.assert(challenge: challenge, allowing: credentials)
    }
}
