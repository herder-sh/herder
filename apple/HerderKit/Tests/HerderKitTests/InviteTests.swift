import AuthenticationServices
import Foundation
import Herder
@testable import HerderKit
import Testing

/// Invite a teammate's link and minting, and joining from one.
@MainActor
struct InviteTests {
    nonisolated static func machine(_ n: Int, name: String? = nil) -> InviteLink.Machine {
        InviteLink.Machine(hosts: ["10.0.4.\(n):7447"], fingerprint: String(repeating: String(format: "%02x", n), count: 32),
                           invite: "tok-\(n)", name: name)
    }

    @Test func aLinkCarriesEveryMachinesInviteAndReadsBack() throws {
        let link = InviteLink(machines: [
            InviteLink.Machine(hosts: ["devbox.ts.net:7447", "192.168.1.20:7447"], fingerprint: "ab12", invite: "t1", name: "devbox"),
            InviteLink.Machine(hosts: ["10.0.4.2:7447"], fingerprint: "cd34", invite: "t2"),
        ], role: .owner, by: "Tomas", label: "Lina")
        #expect(link.string.hasPrefix("herder://invite?by=Tomas&for=Lina&role=owner&host=devbox.ts.net:7447&host=192.168.1.20:7447&fp=ab12&invite=t1&name=devbox&host="))
        #expect(InviteLink(url: try #require(URL(string: link.string))) == link)
        #expect(URL(string: link.string).flatMap(AppLink.init(url:)) == .invite(link))
        #expect(link.grant == "an owner on 2 machines")
        #expect(link.summary == "Owner on 2 machines")
        #expect(link.machines.map(\.title) == ["devbox", "10.0.4.2"])
    }

    @Test func aLinkOfOneMachineNamesIt() throws {
        let url = try #require(URL(string: "herder://invite?host=devbox.lan:7447&fp=ab12&invite=tok"))
        let link = try #require(InviteLink(url: url))
        #expect(link.role == .member && link.by == nil && link.label == nil)
        #expect(link.machines == [InviteLink.Machine(hosts: ["devbox.lan:7447"], fingerprint: "ab12", invite: "tok")])
        #expect(link.grant == "a member on devbox.lan")
    }

    @Test func aLinkMissingWhatAMachineNeedsReadsAsNothing() {
        for text in ["herder://invite", "herder://invite?by=Tomas", "herder://invite?host=a:1&invite=t",
                     "herder://invite?host=a:1&fp=ab", "herder://invite?fp=ab&invite=t",
                     "herder://invite?host=a:1&fp=ab&invite=t&host=b:1", "herder://fleet?host=a:1&fp=ab&invite=t"] {
            #expect(URL(string: text).flatMap(InviteLink.init(url:)) == nil, "\(text)")
        }
    }

    @Test func invitesOnlyToMachinesYouOwnThatAreConnected() {
        let machines = [
            HerderKitTests.machine("devbox", name: "devbox", sessions: []),
            HerderKitTests.machine("shared", name: "shared", sessions: [], role: .member),
            HerderKitTests.machine("asleep", name: "asleep", sessions: [], connection: .connecting),
            HerderKitTests.machine("studio", name: "studio", sessions: []),
        ]
        #expect(InviteMaker.invitable(machines).map(\.hostId) == ["devbox", "studio"])
    }

    nonisolated static let machines = ["devbox", "nas", "studio"].map { InviteMaker.Machine(hostId: $0, name: $0) }

    private let sign: PasskeySigner = { _ in throw HerderError.Local(detail: "unused") }

    @Test func makesOneLinkOfEveryTickedMachinesInvite() async throws {
        var asked: [String] = []
        let maker = InviteMaker(machines: Self.machines) { role, machine, _ in
            asked.append(machine.hostId)
            if machine.hostId == "nas" { throw HerderError.Unreachable(detail: "timed out") }
            return FleetPeople.Invite(role: role, machine: InviteLink.Machine(hosts: ["\(machine.hostId):7447"], fingerprint: machine.hostId,
                                                                             invite: "t", name: machine.name),
                                      expiresAt: Date(timeIntervalSince1970: machine.hostId == "studio" ? 100 : 200))
        }
        #expect(maker.allPicked)
        maker.name = " Lina "
        maker.role = .owner
        maker.by = "Tomas"
        await maker.make(sign: sign)
        #expect(asked == ["devbox", "nas", "studio"])
        let link = try #require(maker.link)
        #expect(link.machines.map(\.name) == ["devbox", "studio"])
        #expect(link.role == .owner && link.label == "Lina" && link.by == "Tomas")
        #expect(maker.expiresAt == Date(timeIntervalSince1970: 100))
        #expect(maker.failures == ["nas: timed out"])
        maker.reset()
        #expect(maker.link == nil && maker.failures.isEmpty)
    }

    @Test func opensOnOneMachineTicksItAlone() async {
        var asked: [String] = []
        let maker = InviteMaker(machines: Self.machines, only: "nas") { role, machine, _ in
            asked.append(machine.hostId)
            return FleetPeople.Invite(role: role, machine: InviteTests.machine(1), expiresAt: nil)
        }
        #expect(maker.picked == ["nas"] && !maker.allPicked)
        await maker.make(sign: sign)
        #expect(asked == ["nas"])
        #expect(maker.link?.label == nil)
    }

    @Test func closingFaceIDStopsMinting() async {
        let maker = InviteMaker(machines: Self.machines) { _, machine, _ in
            if machine.hostId == "devbox" { throw ASAuthorizationError(.canceled) }
            throw HerderError.Local(detail: "no")
        }
        await maker.make(sign: sign)
        #expect(maker.link == nil && maker.failures.isEmpty)
    }

    @Test func theRoleSaysWhatItLetsThemDo() {
        #expect(InviteForm.meaning(.member) == "Drives sessions. Never gets a terminal.")
        #expect(InviteForm.meaning(.owner).contains("terminals"))
    }

    @Test func theExpirySaysItWorksOnce() {
        let now = Date(timeIntervalSince1970: 0)
        #expect(InviteLinkPanel.terms(Date(timeIntervalSince1970: 581), now: now) == "Works once · expires in 9:41")
        #expect(InviteLinkPanel.terms(now, now: now) == "Expired: make a new link")
        #expect(InviteLinkPanel.terms(nil, now: now) == "Works once")
    }

    // MARK: Joining

    nonisolated static let link = InviteLink(machines: (1...4).map { machine($0, name: "m\($0)") }, role: .member,
                                             by: "Tomas", label: "Lina")

    @Test func theFirstMachineThatAnswersMakesThePasskeyAndTheRestTakeIt() async {
        var registered: [String] = []
        var accepted: [String] = []
        let lina = person("p-lina", "Lina", .member)
        let join = InviteJoin(Self.link, known: { $0 == Self.machine(4).fingerprint ? "laptop" : nil }) { machine, name in
            registered.append("\(machine.invite) as \(name)")
            if machine.invite == "tok-1" { throw HerderError.Unreachable(detail: "timed out") }
            return ("host-\(machine.invite)", lina)
        } accept: { machine, me in
            accepted.append("\(machine.invite) as \(me.personId)")
            return "host-\(machine.invite)"
        }
        #expect(join.name == "Lina")
        #expect(join.title == "Tomas invited you to herder")
        #expect(join.subtitle == "As a member on 4 machines.")
        #expect(join.action(biometry: "Face ID") == "Create Passkey and Join")
        #expect(join.hint(biometry: "Face ID") == "Face ID asks once for each machine.")
        #expect(join.rows[3].state == .added("laptop") && join.toJoin == 3)
        join.name = " Lina M "
        await join.run()
        #expect(registered == ["tok-1 as Lina M", "tok-2 as Lina M"])
        #expect(accepted == ["tok-3 as p-lina"])
        #expect(join.rows.map(\.state) == [.unreachable, .joined("host-tok-2"), .joined("host-tok-3"), .added("laptop")])
        #expect(join.title == "You’re on 2 of 3 machines")
        #expect(join.action(biometry: "Face ID") == "Try Again")
        #expect(InviteJoin.State.unreachable.help(by: "Tomas").contains("Tailscale"))
    }

    @Test func aDeviceThatKnowsYouJoinsWithYourPasskey() async {
        var registered = 0
        let me = person("p-tomas", "Tomas", .owner)
        let join = InviteJoin(Self.link, known: { _ in nil }, me: me) { _, _ in
            registered += 1
            return ("", me)
        } accept: { machine, _ in
            if machine.invite == "tok-2" {
                throw HerderError.Rejected(info: ErrorInfo(code: .forbidden, message: "the invite is used or has expired"))
            }
            return machine.title
        }
        #expect(!join.newPasskey)
        #expect(join.action(biometry: "Face ID") == "Join with Face ID")
        await join.run()
        #expect(registered == 0)
        #expect(join.rows.map(\.state) == [.joined("m1"), .refused, .joined("m3"), .joined("m4")])
        #expect(InviteJoin.State.refused.help(by: "Tomas") == "Ask Tomas for a new link.")
    }

    @Test func closingFaceIDStopsAndTryAgainJoinsTheRest() async {
        var cancel = true
        let lina = person("p-lina", "Lina", .member)
        let join = InviteJoin(Self.link, known: { _ in nil }) { machine, _ in
            if cancel { throw ASAuthorizationError(.canceled) }
            return (machine.title, lina)
        } accept: { machine, _ in machine.title }
        await join.run()
        #expect(join.rows.allSatisfy { $0.state == .waiting })
        #expect(join.started && join.me == nil)
        cancel = false
        await join.run()
        #expect(join.pending == 0)
        #expect(join.title == "You’re in")
    }
}
