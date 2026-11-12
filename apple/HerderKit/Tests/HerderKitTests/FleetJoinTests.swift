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

    nonisolated static let link = FleetLink(machines: (1...5).map(machine))

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
        let link = FleetLink(machines: [named, Self.machine(2), blank, Self.machine(4)])
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
                throw HerderError.SignInRefused(refusal: .unknownPerson(owners: ["Tomas"]),
                                                detail: "this passkey's person is not on this daemon")
            case "10.0.4.4:7447": throw HerderError.Rejected(info: ErrorInfo(code: .badRequest, message: "this device is paired already"))
            default: return "ok"
            }
        }
        #expect(join.summary(biometry: "Face ID") == "Face ID asks once for each machine.")
        await join.run()
        #expect(join.rows.map(\.state) == [
            .signedIn("ok"), .unreachable, .unknownPerson(owners: ["Tomas"]), .failed("this device is paired already"),
            .signedIn("ok"),
        ])
        #expect(join.summary(biometry: "Face ID") == "Signed in to 2 of 5. 3 need you.")
        #expect(FleetJoin.State.unreachable.help(biometry: "Face ID").contains("Tailscale"))
    }

    /// Each reason a machine gives for refusing the passkey reads as what to do about it.
    @Test func saysWhyAMachineRefusedThePasskey() {
        func state(_ refusal: SignInRefusal) -> FleetJoin.State {
            FleetJoin.state(of: HerderError.SignInRefused(refusal: refusal, detail: "refused"))
        }
        let other = state(.otherPasskey(passkeys: [
            RefusedPasskey(label: "Tomas", aaguid: "bada5566-a7aa-401f-bd96-45619a55120d"),
            RefusedPasskey(label: "Tomas", aaguid: "BADA5566-A7AA-401F-BD96-45619A55120D"),
            RefusedPasskey(label: "Tomas", aaguid: nil),
        ]))
        #expect(other == .otherPasskey("Tomas in 1Password"))
        #expect(other.title(biometry: "Face ID") == "Knows you by another passkey: Tomas in 1Password")
        #expect(FleetJoin.passkeyPlaces([RefusedPasskey(label: "Tomas", aaguid: nil)]) == "Tomas")
        #expect(FleetJoin.passkeyPlaces([]) == "another passkey")

        let stranger = state(.unknownPerson(owners: ["Tomas", "Ada"]))
        #expect(stranger.title(biometry: "Face ID") == "Ask Tomas to add you")
        #expect(stranger.help(biometry: "Face ID").contains("Ada"))
        let nobody = state(.unknownPerson(owners: []))
        #expect(nobody.title(biometry: "Face ID") == "Ask its owner to add you")
        #expect(nobody.help(biometry: "Face ID").contains("herder claim"))

        let skipped = state(.userNotVerified)
        #expect(skipped == .notVerified)
        #expect(skipped.title(biometry: "Face ID") == "Face ID was skipped, try again")
        #expect(skipped.title(biometry: "Touch ID") == "Touch ID was skipped, try again")
        #expect([other, stranger, skipped].allSatisfy(\.pending))

        // Any other refusal says what the machine said.
        let forbidden = HerderError.Rejected(info: ErrorInfo(code: .forbidden, message: "the passkey's signature does not verify"))
        #expect(FleetJoin.state(of: forbidden) == .failed("the passkey's signature does not verify"))
        #expect(passkeyStore(aaguid: "00000000-0000-0000-0000-000000000000") == nil)
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

        let down = FleetMachine(hosts: ["127.0.0.1:1"], fingerprint: String(repeating: "ef", count: 32))
        let link = FleetLink(machines: [
            try parseFleetLink(link: mine.link).machines[0],
            try parseFleetLink(link: theirs.link).machines[0],
            down,
            try parseFleetLink(link: added.link).machines[0],
        ])
        let shared = try #require(pairLink(in: fleetLinkToString(link: link)))
        guard case .fleet(let parsed) = shared else { Issue.record("not a fleet link"); return }

        // Every fake daemon knows the same recorded passkey: their-box gets one nobody has.
        let stranger = try #require(RecordedPasskey([
            Data((0..<16).map { _ in UInt8.random(in: 0...255) }), P256.Signing.PrivateKey().derRepresentation,
            Data((0..<16).map { _ in UInt8.random(in: 0...255) }),
        ].map { $0.base64URLEncodedString() }.joined(separator: ".")))
        let theirFingerprint = try parseFleetLink(link: theirs.link).machines[0].fingerprint
        let join = FleetJoin(parsed, known: { fleet.machine(fingerprint: $0)?.name }) {
            try await fleet.join($0, with: $0.fingerprint == theirFingerprint ? stranger : passkey)
        }
        #expect(join.title == "Sign in to 3 machines as you")
        await join.run()
        #expect(join.rows[0].state == .signedIn("devbox"))
        guard case .unknownPerson(let owners) = join.rows[1].state else {
            Issue.record("expected their-box not to know the passkey, got \(join.rows[1].state)")
            return
        }
        #expect(!owners.isEmpty)
        #expect(join.rows[2...].map(\.state) == [.unreachable, .added("laptop")])
        #expect(Set(fleet.client.machines().map(\.name)) == ["devbox", "laptop"])
    }
}
