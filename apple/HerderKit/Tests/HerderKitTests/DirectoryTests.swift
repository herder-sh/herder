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

    @Test func machinesUsersMergeIntoOnePersonEach() {
        let users: [HostId: [UserInfo]] = [
            "devbox": [
                UserInfo(userId: "u1", name: "tomas", role: .owner, devices: [
                    PeopleTests.device("d1", "herder-macos/0.17.0", online: true, lastSeen: "2026-10-29T11:00:00Z"),
                ]),
                UserInfo(userId: "u2", name: "sam", role: .member, devices: []),
                UserInfo(userId: "u3", name: "studio", role: .member, devices: [
                    PeopleTests.device("d2", "herder/0.17.0", online: true, hostOnly: true),
                ]),
            ],
            "laptop": [
                UserInfo(userId: "u4", name: "pat", role: .owner, devices: []),
                UserInfo(userId: "u5", name: "Tomas", role: .member, devices: [
                    PeopleTests.device("d3", "herder-ios/0.17.0", lastSeen: "2026-10-29T09:00:00Z"),
                ]),
            ],
        ]
        let directory = Directory.merging(users, columns: [.init(id: "devbox", name: "devbox"), .init(id: "laptop", name: "laptop")],
                                          me: "tomas", thisClient: "herder-macos/0.17.0")
        #expect(directory.me.roles == ["devbox": .owner, "laptop": .member])
        #expect(directory.teammates.map(\.name) == ["Sam", "Pat"])
        #expect(directory.devices.map(\.title) == ["This Mac", "iPhone"])
    }
}

@MainActor
struct FleetPeopleTests {
    nonisolated static let users: [HostId: [UserInfo]] = [
        "devbox": [
            UserInfo(userId: "u1", name: "tomas", role: .owner, devices: [
                PeopleTests.device("d1", FleetPeople.thisClient + "/0.17.0", online: true),
            ]),
            UserInfo(userId: "u2", name: "sam", role: .member, devices: [
                PeopleTests.device("d2", FleetPeople.thisClient + "/0.17.0", online: true),
            ]),
        ],
        "laptop": [
            UserInfo(userId: "u3", name: "pat", role: .owner, devices: []),
            UserInfo(userId: "u4", name: "tomas", role: .member, devices: [
                PeopleTests.device("d3", FleetPeople.thisClient + "/0.17.0", online: true),
            ]),
            UserInfo(userId: "u5", name: "sam", role: .member, devices: []),
        ],
    ]

    let machines = [machine("devbox", name: "devbox", sessions: []),
                    machine("laptop", name: "laptop", sessions: [], role: .member)]

    private func model() -> (FleetPeople, () -> [CommandBody]) {
        var sent: [CommandBody] = []
        let people = FleetPeople { hostId, command in
            if command == .listUsers { return .users(users: Self.users[hostId] ?? []) }
            sent.append(command)
            return .applied
        }
        return (people, { sent })
    }

    @Test func youAreWhoThisAppPairsAsWithTheMachinesRole() {
        #expect(FleetPeople.me(Self.users, machines: machines) == "tomas")
    }

    @Test func theDirectoryHasAColumnPerMachineThatListedItsUsers() async throws {
        let (people, _) = model()
        await people.reload(machines)
        let directory = try #require(people.directory(machines, localNames: []))
        #expect(directory.columns.map(\.name) == ["devbox", "laptop"])
        #expect(directory.me.name == "Tomas")
        #expect(directory.teammates.map(\.name) == ["Sam", "Pat"])
        let laptop = directory.only("laptop")
        #expect(laptop.columns.map(\.id) == ["laptop"])
        #expect(laptop.teammates.map(\.name) == ["Sam", "Pat"])
        #expect(directory.only("devbox").teammates.map(\.name) == ["Sam"])
    }

    @Test func aRoleChangeGoesToEachMachineTheyAreOn() async throws {
        let (people, sent) = model()
        await people.reload(machines)
        let directory = try #require(people.directory(machines, localNames: []))
        let sam = directory.teammates[0]
        try await people.setRole(.owner, of: sam, on: directory.columns, machines: machines)
        #expect(sent() == [.setUserRole(userId: "u2", role: .owner), .setUserRole(userId: "u5", role: .owner)])
        let pat = directory.teammates[1]
        await #expect(throws: HerderError.self) {
            try await people.setRole(.member, of: pat, on: directory.columns, machines: machines)
        }
    }
}
