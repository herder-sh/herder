import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct FleetTests {
    @Test func aProfileThatCannotBeOpenedSaysWhy() throws {
        let file = temporaryProfile()
        try Data().write(to: file)
        guard case .failed(let message) = Profile.open(at: file, client: "test") else {
            Issue.record("opened a profile at a file")
            return
        }
        #expect(!message.isEmpty)
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func pairingShowsTheMachineConnected() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        #expect(fleet.machines.isEmpty)
        let following = Task { await fleet.follow() }
        defer { following.cancel() }

        let machine = try await fleet.pair(link: daemon.link)
        #expect(machine.name == "fake-host")
        #expect(fleet.machines.map(\.hostId) == [machine.hostId])
        #expect(await eventually { fleet.machines.first?.connection == .connected })
        #expect(await eventually { fleet.machines.first?.role == .owner })
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func followingShowsAConnectionMadeBeforeIt() async throws {
        let daemon = try FakeDaemon()
        let profile = temporaryProfile()
        do {
            guard case .opened(let fleet) = Profile.open(at: profile, client: "test") else {
                Issue.record("cannot open a fresh profile")
                return
            }
            try await fleet.pair(link: daemon.link)
            fleet.suspend()
        }
        // Reopened, as on an app launch: the machine connects before the view follows.
        guard case .opened(let fleet) = Profile.open(at: profile, client: "test") else {
            Issue.record("cannot reopen the profile")
            return
        }
        let host = try #require(fleet.machines.first?.hostId)
        try await fleet.client.synced(hostId: host)
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        #expect(await eventually { fleet.machines.first?.connection == .connected })
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aSessionStartedFromTheAppShowsLiveAndArchives() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let machine = try await fleet.pair(link: daemon.link)
        try await fleet.client.synced(hostId: machine.hostId)

        let key = try await fleet.createSession(
            on: machine.hostId, repo: daemon.repo, projectId: nil, accountId: daemon.account, model: "",
            mode: .ask, prompt: "Say hello.")
        let sessionId = key.sessionId

        #expect(await eventually {
            fleet.lists.recent.contains { $0.key.sessionId == sessionId && $0.activity == "Hello, world." }
        })
        #expect(fleet.lists.projects.flatMap(\.sessions).map(\.key.sessionId) == [sessionId])

        await fleet.archive(key)
        #expect(await eventually { fleet.sessions[key]?.state == .archived })
        #expect(fleet.refusals[key] == nil)
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aFullTurnShowsInTheTranscript() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let machine = try await fleet.pair(link: daemon.link)
        try await fleet.client.synced(hostId: machine.hostId)
        let key = try await fleet.createSession(
            on: machine.hostId, repo: daemon.repo, projectId: nil, accountId: daemon.account, model: "",
            mode: .fullAccess, prompt: "Say hello.")

        #expect(await eventually {
            guard let model = fleet.sessions[key] else { return false }
            return Transcript.blocks(model).contains { $0 == .assistant(id: $0.id, text: "Hello, world.", streaming: false) }
        })
        let blocks = Transcript.blocks(try #require(fleet.sessions[key]))
        #expect(blocks.contains { if case .user(_, "Say hello.", false) = $0 { true } else { false } })
    }
}
