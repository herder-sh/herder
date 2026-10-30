import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct DemoTests {
    /// The bundled demo opens with no daemon: its machines are connected, the Board has their
    /// tasks, and a command says it is a demo instead of going anywhere.
    @Test func theDemoShowsAFleetAndSendsNothing() async throws {
        let fleet = try Fleet.demo()
        #expect(fleet.machines.map(\.name) == ["devbox", "studio", "laptop", "vault"])
        #expect(fleet.machines.allSatisfy { $0.connection == .connected })
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let key = try #require(fleet.machines.first?.sessions.first.map {
            SessionKey(hostId: fleet.machines[0].hostId, sessionId: $0.sessionId)
        })
        for _ in 0..<50 where fleet.session(key)?.loaded != true {
            try await Task.sleep(for: .milliseconds(100))
        }
        #expect(fleet.session(key)?.loaded == true)
        #expect(!fleet.lists.board(matching: "").isEmpty)

        await fleet.send(.interrupt(sessionId: key.sessionId), about: key)
        #expect(fleet.refusals[key] == "This is a demo; nothing is sent.")
        guard case .people(let people) = try await fleet.client.send(hostId: key.hostId, command: .listPeople) else {
            Issue.record("the demo has no recorded people")
            return
        }
        #expect(!people.isEmpty)
    }
}
