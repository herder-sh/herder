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

    @Test func aMachineThatCannotListShowsWhy() async {
        let people = People(hostId: "devbox") { _ in
            throw HerderError.Rejected(info: ErrorInfo(code: .forbidden, message: "no"))
        }
        await people.reload()
        guard case .failed(let why) = people.users else { Issue.record("not failed"); return }
        #expect(!why.isEmpty)
    }
}

extension Load {
    var isAsking: Bool { if case .asking = self { true } else { false } }
}
