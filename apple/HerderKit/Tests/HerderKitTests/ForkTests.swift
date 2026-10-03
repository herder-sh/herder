import Foundation
import Herder
@testable import HerderKit
import Testing

struct ForkTests {
    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func forkCreatesAnotherSessionAndPreservesTheSource() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let host = try await fleet.pair(link: daemon.link)
        let original = try await fleet.createSession(
            on: host.hostId, repo: daemon.repo, projectId: nil, accountId: daemon.account, model: "",
            mode: .ask, prompt: "hello")
        #expect(await eventually { fleet.sessions[original]?.lastMessage != nil && fleet.sessions[original]?.turn == nil })
        let originalBranch = fleet.sessions[original]?.branch
        let originalStatus = fleet.sessions[original]?.status
        let flow = ForkSessionModel()
        let destination = try #require(fleet.machines.first { $0.hostId == host.hostId })
        let provider = try #require(fleet.sessions[original]?.provider)
        flow.select(destination, provider: provider)
        await flow.fork(source: original, provider: provider, machines: fleet.machines) { hostId, command in
            try await fleet.client.send(hostId: hostId, command: command)
        }
        #expect(flow.error == nil)
        let fork = try #require(flow.opened)
        #expect(fork.hostId == original.hostId)
        #expect(fork.sessionId != original.sessionId)
        #expect(await eventually { fleet.sessions[fork]?.loaded == true })
        #expect(fleet.sessions[original]?.branch == originalBranch)
        #expect(fleet.sessions[original]?.status == originalStatus)
        #expect(fleet.sessions[fork]?.branch != originalBranch)
        #expect(flow.origin == ForkOrigin(sessionId: original.sessionId, hostId: host.hostId))
    }
}
