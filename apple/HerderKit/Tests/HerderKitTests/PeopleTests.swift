import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct PeopleTests {
    /// A machine that records what it is sent, lists `users`, mints codes, and refuses with
    /// `refusal` when set.
    @MainActor final class Machine {
        var sent: [CommandBody] = []
        var users: [UserInfo] = PeopleTests.team
        var refusal: ErrorInfo?

        func model() -> People {
            People(hostId: "devbox") { [self] command in
                sent.append(command)
                if let refusal, command != .listUsers { throw HerderError.Rejected(info: refusal) }
                switch command {
                case .listUsers: return .users(users: users)
                case .pairDevice:
                    return .devicePairing(code: "ABCDE-FGHJK", fingerprint: "ab12", addresses: ["devbox.lan:7447"],
                                          expiresAt: "2026-10-26T12:10:00Z")
                default: return .applied
                }
            }
        }

        var changes: [CommandBody] { sent.filter { $0 != .listUsers } }
    }

    nonisolated static func device(_ id: String, _ client: String, online: Bool = false, lastSeen: String? = nil,
                                   hostOnly: Bool = false) -> UserDevice {
        UserDevice(deviceId: id, client: client, pairedAt: "2026-10-03T12:00:00Z", lastSeen: lastSeen, online: online,
                   hostOnly: hostOnly)
    }

    nonisolated static let team = [
        UserInfo(userId: "u-alice", name: "alice", role: .owner, devices: [
            device("d-mac", "herder-macos/0.6.0", online: true, lastSeen: "2026-10-26T09:00:00Z"),
            device("d-phone", "herder-ios/0.6.0", lastSeen: "2026-10-26T07:00:00Z"),
        ]),
        UserInfo(userId: "u-bob", name: "bob", role: .member, devices: [
            device("d-bob", "herder-ios/0.6.0", lastSeen: "2026-10-24T09:00:00Z"),
        ]),
        UserInfo(userId: "u-devbox", name: "devbox", role: .member, devices: [
            device("d-host", "herder/0.6.0", online: true, hostOnly: true),
        ]),
        UserInfo(userId: "u-carol", name: "carol", role: .member, devices: []),
    ]

    @Test func theListLeavesOutHostsThatOnlyBackUp() async {
        let machine = Machine()
        let people = machine.model()
        #expect(people.users.isAsking)
        await people.reload()
        guard case .loaded(let users) = people.users else { Issue.record("not listed"); return }
        #expect(users.map(\.name) == ["alice", "bob", "carol"])
    }

    @Test func eachActionSendsItsCommandAndListsAgain() async throws {
        let machine = Machine()
        let people = machine.model()
        await people.reload()
        try await people.setRole(.owner, of: Self.team[1])
        try await people.revoke(Self.team[0].devices[1])
        let invite = try await people.invite(" dana ", as: .owner)
        #expect(machine.changes == [
            .setUserRole(userId: "u-bob", role: .owner),
            .revokeDevice(deviceId: "d-phone", assertion: nil),
            .pairDevice(user: "dana", role: .owner),
        ])
        #expect(machine.sent.count == 1 + 3 * 2)
        #expect(invite.name == "dana" && invite.role == .owner)
        #expect(invite.link == pairingUriToString(uri: PairingUri(hosts: ["devbox.lan:7447"], fingerprint: "ab12",
                                                                  code: "ABCDE-FGHJK")))
        #expect(invite.expiresAt == Timestamp.date("2026-10-26T12:10:00Z"))
    }

    @Test func anInviteNeedsAName() async {
        let machine = Machine()
        let people = machine.model()
        await #expect(throws: HerderError.self) { _ = try await people.invite("  ", as: .member) }
        #expect(machine.sent.isEmpty)
    }

    @Test func aRefusalReachesTheCallerAndTheListStays() async {
        let machine = Machine()
        let people = machine.model()
        await people.reload()
        machine.refusal = ErrorInfo(code: .conflict, message: "the last owner cannot be made a member")
        await #expect(throws: HerderError.self) { try await people.setRole(.member, of: Self.team[0]) }
        guard case .loaded(let users) = people.users else { Issue.record("not listed"); return }
        #expect(users.count == 3)
    }

    @Test func aMachineThatCannotListShowsWhy() async {
        let people = People(hostId: "devbox") { _ in
            throw HerderError.Rejected(info: ErrorInfo(code: .forbidden, message: "no"))
        }
        await people.reload()
        guard case .failed(let why) = people.users else { Issue.record("not failed"); return }
        #expect(!why.isEmpty)
    }

    @Test func onlyTheOnlyOwnerIsTheLastOne() {
        let users = People.people(Self.team)
        #expect(People.isLastOwner(users[0], in: users))
        #expect(!People.isLastOwner(users[1], in: users))
        var two = users
        two[1].role = .owner
        #expect(!People.isLastOwner(two[0], in: two))
    }

    @Test func devicesAreNamedAndDescribed() {
        #expect(People.deviceName("herder-ios/0.6.0") == "iPhone or iPad")
        #expect(People.deviceName("herder-macos/dev") == "Mac")
        #expect(People.deviceName("herder-tui/0.13.0") == "Terminal")
        #expect(People.deviceName("something-else") == "something-else")
        let now = Timestamp.date("2026-10-26T09:00:00Z")!
        let devices = Self.team[0].devices
        #expect(People.detail(devices[0], now: now).hasPrefix("Online · v0.6.0 · paired "))
        #expect(People.detail(devices[1], now: now).hasPrefix("Last seen 2h ago · v0.6.0"))
        #expect(People.detail(Self.device("d", "test"), now: now).hasPrefix("Not seen yet · paired "))
    }
}

extension Load {
    var isAsking: Bool { if case .asking = self { true } else { false } }
}
