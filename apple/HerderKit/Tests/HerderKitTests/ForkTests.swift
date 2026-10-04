import Foundation
import Herder
@testable import HerderKit
import Testing

struct ForkTests {
    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func successfulForkNavigatesToTheReplicatedResultAndPreservesTheSource() async throws {
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
            mode: .ask, prompt: "Say hello.")
        #expect(await eventually { fleet.sessions[original]?.lastMessage != nil && fleet.sessions[original]?.status == .idle })
        let originalBranch = fleet.sessions[original]?.branch
        let originalStatus = fleet.sessions[original]?.status
        let originalLog = try #require(fleet.sessions[original]?.log)
        let originalTimeline = try #require(fleet.sessions[original]?.timeline)
        // `synced` ensures the original's head notification has arrived before comparison.
        try await fleet.client.synced(hostId: host.hostId)
        let originalHead = try #require(fleet.client.machines().first { $0.hostId == host.hostId }?
            .sessions.first { $0.sessionId == original.sessionId })
        let flow = ForkSessionModel()
        let destination = try #require(fleet.machines.first { $0.hostId == host.hostId })
        let provider = try #require(fleet.sessions[original]?.provider)
        flow.select(destination, provider: provider)
        var navigated: [SessionKey] = []
        await flow.forkAndOpen(source: original, fleet: fleet) { navigated.append($0) }
        #expect(flow.error == nil)
        #expect(!flow.working)
        let fork = try #require(navigated.first)
        #expect(navigated.count == 1)
        #expect(fork == flow.opened)
        #expect(fork.hostId == host.hostId)
        #expect(fork.sessionId != original.sessionId)
        // The fork's history can arrive over more than one update: wait for all of it.
        #expect(await eventually {
            fleet.sessions[fork].map { $0.loaded && $0.lastMessage != nil && $0.status == .idle } == true
        })
        let copy = try #require(fleet.sessions[fork])
        #expect(copy.lastMessage == "Hello, world.")
        #expect(copy.branch != originalBranch)
        #expect(copy.status == .idle)
        #expect(fleet.forkOrigins[fork] == ForkOrigin(sessionId: original.sessionId, hostId: host.hostId))
        try await fleet.client.synced(hostId: host.hostId)
        #expect(fleet.client.machines().first { $0.hostId == host.hostId }?
            .sessions.first { $0.sessionId == original.sessionId } == originalHead)
        #expect(fleet.sessions[original]?.log == originalLog)
        #expect(fleet.sessions[original]?.timeline == originalTimeline)
        #expect(fleet.sessions[original]?.branch == originalBranch)
        #expect(fleet.sessions[original]?.status == originalStatus)
    }
}
