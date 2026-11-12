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

    /// Reaches every machine but the ones in `down`.
    static func reach(down: Set<String> = []) -> FleetJoin.Check {
        { machine in
            if down.contains(machine.hosts[0]) { throw HerderError.Unreachable(detail: "\(machine.hosts[0]): timed out") }
        }
    }

    @Test func skipsTheMachinesThisDeviceHasAlready() async {
        var asked: [String] = []
        let join = FleetJoin(Self.link, known: { $0 == Self.machine(2).fingerprint ? "devbox" : nil },
                             check: Self.reach()) { machine in
            asked.append(machine.fingerprint)
            return "host-\(machine.hosts[0])"
        }
        #expect(join.rows[1].state == .added("devbox"))
        #expect(join.title(device: "iPhone") == "Sign in to your 4 machines")
        await join.check()
        await join.run()
        #expect(!asked.contains(Self.machine(2).fingerprint) && asked.count == 4)
        #expect(join.rows[1].state == .added("devbox"))
        #expect(join.rows[0].state == .signedIn("host-10.0.4.1:7447"))
        #expect(join.pending == 0 && join.step == 3)
        #expect(join.title(device: "iPhone") == "This iPhone is on all 5 machines")
        #expect(join.hint(biometry: "Face ID") == "")
    }

    @Test func namesEachMachineAsTheLinkDoesElseByItsAddress() async {
        var named = Self.machine(1)
        named.name = "alice’s vault"
        var blank = Self.machine(3)
        blank.name = ""
        let link = FleetLink(machines: [named, Self.machine(2), blank, Self.machine(4)])
        let join = FleetJoin(link, known: { $0 == Self.machine(4).fingerprint ? "devbox" : nil },
                             check: Self.reach(down: ["10.0.4.1:7447"])) { machine in "host-\(machine.hosts[0])" }
        #expect(join.rows.map(\.name) == ["alice’s vault", "10.0.4.2", "10.0.4.3", "devbox"])
        await join.check()
        await join.run()
        // Signing in names it as the machine does; one it could not reach keeps the link's name.
        #expect(join.rows.map(\.name) == ["alice’s vault", "host-10.0.4.2:7447", "host-10.0.4.3:7447", "devbox"])
        #expect(FleetJoin.name(named) == "alice’s vault")
        #expect(FleetJoin.name(Self.machine(2)) == "10.0.4.2")
    }

    /// Reachability is known before Face ID: the machine that cannot be reached says so, with
    /// its cause, while the rest wait ready, and signing in asks only for those.
    @Test func reachesEachMachineBeforeFaceID() async {
        var asked: [String] = []
        let tailnet = FleetMachine(hosts: ["100.71.3.22:7447"], fingerprint: String(repeating: "aa", count: 32), name: "mini-k8s")
        let join = FleetJoin(FleetLink(machines: [Self.machine(1), tailnet, Self.machine(3)]), known: { _ in nil },
                             check: Self.reach(down: ["100.71.3.22:7447"])) { machine in
            asked.append(machine.hosts[0])
            return "ok"
        }
        #expect(join.rows.map(\.state) == [.checking, .checking, .checking])
        #expect(join.hint(biometry: "Face ID") == "Reaching each machine first…")
        await join.check()
        #expect(asked.isEmpty)
        #expect(join.rows.map(\.state) == [.ready, .unreachable, .ready])
        #expect(join.rows[1].status(device: "iPhone", biometry: "Face ID") == "Can’t reach it: Tailscale may be off on this iPhone")
        #expect(join.rows[1].fix(biometry: "Face ID") == "Turn Tailscale on, then try again.")
        #expect(join.hint(biometry: "Face ID") == "Face ID asks once for each machine it can reach. mini-k8s can wait.")
        await join.run()
        #expect(asked == ["10.0.4.1:7447", "10.0.4.3:7447"])
        #expect(join.title(device: "iPhone") == "Signed in to 2 of 3")
        #expect(join.subtitle == "One needs you. Fix it here, or come back to it later.")
        #expect(join.hint(biometry: "Face ID") == "Left to do: mini-k8s. Scan the code again to finish.")
        #expect(join.step == 1)
    }

    @Test func saysWhyEachMachineDidNotSignIn() async {
        let join = FleetJoin(Self.link, known: { _ in nil }, check: Self.reach(down: ["10.0.4.2:7447"])) { machine in
            switch machine.hosts[0] {
            case "10.0.4.3:7447":
                throw HerderError.SignInRefused(refusal: .unknownPerson(owners: ["Tomas"]), detail: "not on this daemon")
            case "10.0.4.4:7447": throw HerderError.Rejected(info: ErrorInfo(code: .badRequest, message: "this device is paired already"))
            default: return "ok"
            }
        }
        await join.check()
        await join.run()
        #expect(join.rows.map(\.state) == [
            .signedIn("ok"), .unreachable, .unknownPerson(owners: ["Tomas"]), .failed("this device is paired already"),
            .signedIn("ok"),
        ])
        #expect(join.title(device: "iPhone") == "Signed in to 2 of 5")
        #expect(join.subtitle == "Three need you. Fix them here, or come back to them later.")
        #expect(join.hint(biometry: "Face ID") == "Left to do: 10.0.4.2, 10.0.4.3 and 10.0.4.4. Scan the code again to finish.")
        #expect(join.rows[1].status(device: "iPhone", biometry: "Face ID") == "Can’t reach it: this iPhone may be on another network")
        #expect(join.rows[2].status(device: "iPhone", biometry: "Face ID") == "Ask Tomas to add you")
        #expect(join.rows[3].status(device: "iPhone", biometry: "Face ID") == "Couldn’t sign in: this device is paired already")
    }

    /// Each reason a machine gives for refusing the passkey reads as what to do about it.
    @Test func saysWhyAMachineRefusedThePasskey() {
        func row(_ refusal: SignInRefusal) -> FleetJoin.Row {
            FleetJoin.Row(machine: Self.machine(1),
                          state: FleetJoin.state(of: HerderError.SignInRefused(refusal: refusal, detail: "refused")))
        }
        func status(_ row: FleetJoin.Row, _ biometry: String = "Face ID") -> String {
            row.status(device: "iPhone", biometry: biometry)
        }
        let other = row(.otherPasskey(passkeys: [
            RefusedPasskey(label: "Tomas", aaguid: "bada5566-a7aa-401f-bd96-45619a55120d"),
            RefusedPasskey(label: "Tomas", aaguid: "BADA5566-A7AA-401F-BD96-45619A55120D"),
            RefusedPasskey(label: "Tomas", aaguid: nil),
        ]))
        #expect(other.state == .otherPasskey("Tomas in 1Password"))
        #expect(status(other) == "Knows you by another passkey: Tomas in 1Password")
        #expect(FleetJoin.passkeyPlaces([RefusedPasskey(label: "Tomas", aaguid: nil)]) == "Tomas")
        #expect(FleetJoin.passkeyPlaces([]) == "another passkey")

        let stranger = row(.unknownPerson(owners: ["Tomas", "Ada"]))
        #expect(status(stranger) == "Ask Tomas to add you")
        #expect(stranger.fix(biometry: "Face ID").contains("Ada"))
        let nobody = row(.unknownPerson(owners: []))
        #expect(status(nobody) == "Ask its owner to add you")
        #expect(nobody.fix(biometry: "Face ID").contains("herder claim"))

        let skipped = row(.userNotVerified)
        #expect(skipped.state == .notVerified)
        #expect(status(skipped) == "Face ID was skipped, try again")
        #expect(status(skipped, "Touch ID") == "Touch ID was skipped, try again")
        #expect([other, stranger, skipped].allSatisfy { $0.state.pending && $0.state.needsYou })

        // Any other refusal says what the machine said.
        let forbidden = HerderError.Rejected(info: ErrorInfo(code: .forbidden, message: "the passkey's signature does not verify"))
        #expect(FleetJoin.state(of: forbidden) == .failed("the passkey's signature does not verify"))
        #expect(passkeyStore(aaguid: "00000000-0000-0000-0000-000000000000") == nil)
    }

    /// Try Again on one row reaches it again and, once signing in has started, signs in to it
    /// alone.
    @Test func tryAgainFixesOneMachine() async {
        var down: Set<String> = ["10.0.4.2:7447"]
        var asked: [String] = []
        let join = FleetJoin(FleetLink(machines: (1...3).map(Self.machine)), known: { _ in nil }, check: { machine in
            if down.contains(machine.hosts[0]) { throw HerderError.Unreachable(detail: "down") }
        }) { machine in
            asked.append(machine.hosts[0])
            return "ok"
        }
        await join.check()
        // Before signing in, Try Again only reaches it.
        down = []
        await join.retry(Self.machine(2).fingerprint)
        #expect(join.rows[1].state == .ready && asked.isEmpty)
        await join.run()
        #expect(asked == ["10.0.4.1:7447", "10.0.4.2:7447", "10.0.4.3:7447"])
        #expect(join.pending == 0)

        down = ["10.0.4.2:7447"]
        let later = FleetJoin(FleetLink(machines: (1...2).map(Self.machine)), known: { _ in nil },
                              check: { machine in
                                  if down.contains(machine.hosts[0]) { throw HerderError.Unreachable(detail: "down") }
                              }) { _ in "ok" }
        await later.check()
        await later.run()
        #expect(later.rows.map(\.state) == [.signedIn("ok"), .unreachable])
        down = []
        await later.retry(Self.machine(2).fingerprint)
        #expect(later.rows.map(\.state) == [.signedIn("ok"), .signedIn("ok")])
        // A row that does not need you is left alone.
        await later.retry(Self.machine(1).fingerprint)
        #expect(later.rows[0].state == .signedIn("ok"))
    }

    @Test func closingFaceIDStopsAndSigningInAgainSignsInToTheRest() async {
        var cancel = true
        var asked: [String] = []
        let join = FleetJoin(Self.link, known: { _ in nil }, check: Self.reach(down: ["10.0.4.1:7447"])) { machine in
            asked.append(machine.hosts[0])
            if machine.hosts[0] == "10.0.4.3:7447", cancel { throw ASAuthorizationError(.canceled) }
            return "ok"
        }
        await join.check()
        await join.run()
        #expect(join.rows.map(\.state) == [.unreachable, .signedIn("ok"), .ready, .ready, .ready])
        #expect(asked == ["10.0.4.2:7447", "10.0.4.3:7447"])

        cancel = false
        asked = []
        await join.run()
        // Signed in stays signed in; what was left ready is signed in to.
        #expect(asked == ["10.0.4.3:7447", "10.0.4.4:7447", "10.0.4.5:7447"])
        #expect(join.rows.map(\.state) == [.unreachable, .signedIn("ok"), .signedIn("ok"), .signedIn("ok"), .signedIn("ok")])
    }

    @Test func knowsATailscaleAddress() {
        func on(_ host: String) -> Bool { FleetJoin.onTailscale(FleetMachine(hosts: [host], fingerprint: "")) }
        #expect(on("100.64.0.1:7447") && on("100.127.255.1") && on("box.tail1234.ts.net:7447") && on("[fd7a:115c:a1e0::1]:7447"))
        #expect(!on("100.128.0.1:7447") && !on("192.168.1.20:7447") && !on("devbox.local"))
        #expect(FleetJoin.list(["a", "b", "c"]) == "a, b and c")
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
        let join = FleetJoin(parsed, known: { fleet.machine(fingerprint: $0)?.name },
                             check: { try await fleet.reach($0) }) {
            try await fleet.join($0, with: $0.fingerprint == theirFingerprint ? stranger : passkey)
        }
        #expect(join.title(device: "iPhone") == "Sign in to your 3 machines")
        await join.check()
        // Reached before any passkey is asked; checking saves nothing.
        #expect(join.rows.map(\.state) == [.ready, .ready, .unreachable, .added("laptop")])
        #expect(fleet.client.machines().map(\.name) == ["laptop"])
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
