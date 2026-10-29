import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct PeopleTests {
    nonisolated static func device(_ id: String, _ client: String, online: Bool = false, lastSeen: String? = nil,
                                   hostOnly: Bool = false) -> UserDevice {
        UserDevice(deviceId: id, client: client, pairedAt: "2026-10-03T12:00:00Z", lastSeen: lastSeen, online: online,
                   hostOnly: hostOnly)
    }

    nonisolated static let team = [
        PersonInfo(personId: "p-alice", name: "alice", role: .owner, passkeys: [], devices: []),
        PersonInfo(personId: "p-bob", name: "bob", role: .member, passkeys: [], devices: []),
    ]

    @Test func listsTheMachinesPeople() async {
        var sent: [CommandBody] = []
        let people = People(hostId: "devbox") { command in
            sent.append(command)
            return .people(people: Self.team)
        }
        #expect(people.people.isAsking)
        await people.reload()
        #expect(sent == [.listPeople])
        guard case .loaded(let listed) = people.people else { Issue.record("not listed"); return }
        #expect(listed.map(\.name) == ["alice", "bob"])
    }

    @Test func aMachineThatCannotListShowsWhy() async {
        let people = People(hostId: "devbox") { _ in
            throw HerderError.Rejected(info: ErrorInfo(code: .forbidden, message: "no"))
        }
        await people.reload()
        guard case .failed(let why) = people.people else { Issue.record("not failed"); return }
        #expect(!why.isEmpty)
    }
}

extension Load {
    var isAsking: Bool { if case .asking = self { true } else { false } }
}
