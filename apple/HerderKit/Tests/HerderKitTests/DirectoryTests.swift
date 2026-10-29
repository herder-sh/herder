import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct DirectoryTests {
    private let now = Timestamp.date("2026-10-29T12:00:00Z")!

    @Test func eachPersonGetsTheMockupsRoleAndScope() {
        let directory = Directory.demo(now: now)
        let rows = directory.everyone.map { ($0.name, directory.fleetRole($0).title, directory.scope($0)) }
        #expect(rows.map(\.0) == ["Tomas", "Sam", "Kim"])
        #expect(rows.map(\.1) == ["Owner", "Member", "Custom"])
        #expect(rows.map(\.2) == ["Owner of every machine", "All machines", "Owner on draugelis · member elsewhere"])
        #expect(directory.status == "3 people · 5 machines")
    }

    @Test func aNarrowerMemberIsCustom() {
        var directory = Directory.demo(now: now)
        directory.teammates[0].roles["mac"] = nil
        #expect(directory.fleetRole(directory.teammates[0]) == .custom)
        #expect(directory.scope(directory.teammates[0]) == "Member on trash-can-01 and draugelis")
        directory.teammates[0].roles = [:]
        #expect(directory.scope(directory.teammates[0]) == "No machines yet")
    }

    @Test func gridCellsAreEachPersonsRoleOnEachMachine() {
        let directory = Directory.demo(now: now)
        let kim = directory.teammates[1]
        #expect(directory.columns.map { kim.roles[$0.id] } == [.member, .member, .owner, nil, .owner])
    }

    @Test func devicesSayWhenTheySignedIn() {
        let directory = Directory.demo(now: now)
        #expect(directory.devices.map(\.title) == ["This Mac", "iPhone", "Terminal on trash-can-01"])
        let oct27 = Timestamp.date("2026-10-27T10:00:00Z")!.formatted(.dateTime.month(.abbreviated).day())
        #expect(directory.devices.map { $0.seen(now: now) } == ["today", "2 hours ago", oct27])
    }
}

func person(_ id: String, _ name: String, _ role: Role, passkey: Bool = true, devices: [UserDevice] = []) -> PersonInfo {
    PersonInfo(personId: id, name: name, role: role,
               passkeys: passkey ? [PasskeyKey(credentialId: "cred-\(id)", publicKey: "{}")] : [], devices: devices)
}

@MainActor
struct FleetPeopleTests {
    nonisolated static let mac = FleetPeople.thisClient + "/0.17.0"
    nonisolated static let people: [HostId: [PersonInfo]] = [
        "devbox": [
            person("p-tomas", "Tomas", .owner, devices: [PeopleTests.device("d1", mac, online: true)]),
            person("p-sam", "Sam", .member, devices: [PeopleTests.device("d2", mac, online: true)]),
        ],
        "laptop": [
            person("p-pat", "Pat", .owner),
            person("p-tomas", "Tomas", .member, devices: [
                PeopleTests.device("d3", mac, online: true),
                PeopleTests.device("d4", "herder-ios/0.17.0", lastSeen: "2026-10-29T09:00:00Z"),
            ]),
        ],
    ]

    let machines = [machine("devbox", name: "devbox", sessions: [], personId: "p-tomas"),
                    machine("laptop", name: "laptop", sessions: [], role: .member, personId: "p-tomas")]

    private func model() -> (FleetPeople, () -> [CommandBody]) {
        var sent: [CommandBody] = []
        let people = FleetPeople { hostId, command in
            switch command {
            case .listPeople: return .people(people: Self.people[hostId] ?? [])
            case .passkeyChallenge:
                return .passkeyChallenge(challenge: PasskeyChallenge(rpId: "herder.sh", challenge: "c-\(hostId)",
                                                                     expiresAt: "2026-10-29T12:01:00Z"),
                                         registration: nil)
            case .invite(let role, _):
                sent.append(command)
                return .invite(invite: "tok", role: role, fingerprint: "ab12", addresses: ["devbox.lan:7447"],
                               expiresAt: "2026-10-29T12:10:00Z")
            default:
                sent.append(command)
                return .applied
            }
        }
        return (people, { sent })
    }

    /// Signs as the challenge's machine, so each command shows which challenge it carries.
    private let sign: PasskeySigner = { challenge in
        PasskeyAssertion(id: "cred", rawId: "cred", response: AssertionResponse(
            clientDataJson: challenge.challenge, authenticatorData: "", signature: "", userHandle: nil), kind: "public-key")
    }

    private func assertion(_ host: String) async throws -> PasskeyAssertion {
        try await sign(PasskeyChallenge(rpId: "herder.sh", challenge: "c-\(host)", expiresAt: ""))
    }

    @Test func youAreThePersonTheMachinesSayThisDeviceActsFor() {
        #expect(FleetPeople.me(machines) == "p-tomas")
        // Sam's Mac is online on devbox as this app's client too; only the machine knows who is who.
        let sam = [machine("devbox", name: "devbox", sessions: [], role: .member, personId: "p-sam"),
                   machine("old", name: "old", sessions: [], personId: nil)]
        #expect(FleetPeople.me(sam) == "p-sam")
        #expect(FleetPeople.me([machine("old", name: "old", sessions: [])]) == nil)
    }

    @Test func theDirectoryHasOnePersonPerPersonIdAndAColumnPerMachine() async throws {
        let (people, _) = model()
        await people.reload(machines)
        let directory = try #require(people.directory(machines, localNames: []))
        #expect(directory.columns.map(\.name) == ["devbox", "laptop"])
        #expect(directory.me.name == "Tomas")
        #expect(directory.me.roles == ["devbox": .owner, "laptop": .member])
        #expect(directory.passkey == .stored(in: nil))
        #expect(directory.teammates.map(\.name) == ["Sam", "Pat"])
        #expect(directory.devices.map(\.title) == ["This Mac", "iPhone"])
        #expect(directory.devices[0].paired == ["devbox": ["d1"], "laptop": ["d3"]])
        #expect(directory.only("devbox").teammates.map(\.name) == ["Sam"])
    }

    @Test func aRoleChangeCarriesEachMachinesOwnChallengeAndIntroducesNewcomers() async throws {
        let (people, sent) = model()
        await people.reload(machines)
        let directory = try #require(people.directory(machines, localNames: []))
        let sam = directory.teammates[0]
        try await people.setRole(.member, of: sam, on: directory.columns, machines: machines, sign: sign)
        let card = PersonCard(name: "Sam", passkeys: [PasskeyKey(credentialId: "cred-p-sam", publicKey: "{}")])
        // devbox has Sam as a member already; laptop learns who Sam is.
        #expect(sent() == [.setPersonRole(personId: "p-sam", role: .member, person: card, assertion: try await assertion("laptop"))])
        try await people.setRole(nil, of: sam, on: directory.columns, machines: machines, sign: sign)
        #expect(sent().last == .revokePerson(personId: "p-sam", assertion: try await assertion("devbox")))
    }

    @Test func anInviteIsALinkToTheMachine() async throws {
        let (people, _) = model()
        let invite = try await people.invite(.owner, on: "devbox", sign: sign)
        #expect(invite.role == .owner)
        #expect(invite.link == "herder://invite?host=devbox.lan:7447&fp=ab12&invite=tok")
        #expect(invite.expiresAt == Timestamp.date("2026-10-29T12:10:00Z"))
    }

    @Test func withoutAPasskeyNothingIsSent() async throws {
        let (people, sent) = model()
        await people.reload(machines)
        let directory = try #require(people.directory(machines, localNames: []))
        await #expect(throws: HerderError.self) {
            try await people.setRole(.owner, of: directory.teammates[0], on: directory.columns, machines: machines,
                                     sign: { _ in throw HerderError.Local(detail: "no passkey") })
        }
        #expect(sent().isEmpty)
    }
}
