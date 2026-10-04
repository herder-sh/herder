import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct DoneTests {
    private let key = SessionKey(hostId: "host-a", sessionId: "01A")

    @Test func aTurnEndingUnseenIsDoneUntilOpenedOrTheNextTurn() {
        var done = DoneSessions(file: nil)
        // History a new subscription replays is no news.
        done.observe(key, before: .running, after: .idle, wasLoaded: false, open: false)
        #expect(!done.contains(key))
        // The open session's own turn ending is seen.
        done.observe(key, before: .running, after: .idle, wasLoaded: true, open: true)
        #expect(!done.contains(key))
        // Elsewhere, it is done.
        done.observe(key, before: .running, after: .idle, wasLoaded: true, open: false)
        #expect(done.contains(key))
        // An update that leaves it idle keeps it done; the next turn clears it.
        done.observe(key, before: .idle, after: .idle, wasLoaded: true, open: false)
        #expect(done.contains(key))
        done.observe(key, before: .idle, after: .running, wasLoaded: true, open: false)
        #expect(!done.contains(key))
        // Opening it clears it.
        done.observe(key, before: .running, after: .idle, wasLoaded: true, open: false)
        done.remove(key)
        #expect(!done.contains(key))
    }

    @Test func doneSurvivesARestart() {
        let file = temporaryProfile().appendingPathComponent("cache/done.json")
        var done = DoneSessions(file: file)
        done.observe(key, before: .running, after: .idle, wasLoaded: true, open: false)
        #expect(DoneSessions(file: file).contains(key))
        done.remove(key)
        #expect(!DoneSessions(file: file).contains(key))
    }

    @Test func theRollUpTakesTheMostPressingStateInTheTUIsOrder() {
        let order: [SessionState] = [.needsYou, .error, .done, .running, .waiting, .idle]
        for (index, state) in order.enumerated() {
            // Everything less pressing, either side of it.
            let below = order[(index + 1)...]
            #expect(SessionState.rollup(below + [state] + below.reversed()) == state)
        }
        #expect(SessionState.rollup([.archived, .idle, .moved]) == .idle)
        #expect(SessionState.rollup([]) == nil)
    }

    @Test func aProjectRollsUpItsLiveSessionsWithDone() {
        var finished = Script("01A")
        var idle = Script("01B")
        var archived = Script("01C")
        let sessions = [
            finished.key: finished.model([created(), .sessionStatusChanged(status: .idle, retryAt: nil)]),
            idle.key: idle.model([created()]),
            archived.key: archived.model([
                created(), .sessionStatusChanged(status: .error, retryAt: nil),
                .sessionStatusChanged(status: .archived, retryAt: nil),
            ]),
        ]
        let machines = [machine("host-a", name: "a", sessions: ["01A", "01B", "01C"])]
        let seen = Lists(machines: machines, sessions: sessions)
        #expect(seen.projects.first?.state == .idle)
        let lists = Lists(machines: machines, sessions: sessions, done: [finished.key])
        #expect(lists.projects.first?.state == .done)
        #expect(lists.projects.first?.live.first { $0.key == finished.key }?.state == .done)
        #expect(lists.home.first { $0.key == finished.key }?.state == .done)
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aTurnEndingWhileTheSessionIsNotShownIsDoneUntilShown() async throws {
        let daemon = try FakeDaemon()
        let profile = temporaryProfile()
        let key: SessionKey
        do {
            guard case .opened(let fleet) = Profile.open(at: profile, client: "test") else {
                Issue.record("cannot open a fresh profile")
                return
            }
            let following = Task { await fleet.follow() }
            defer { following.cancel() }
            let machine = try await fleet.pair(daemon)
            try await fleet.client.synced(hostId: machine.hostId)
            // The hold account's turn runs until it is interrupted.
            key = try await fleet.createSession(
                on: machine.hostId, repo: daemon.repo, projectId: nil, accountId: daemon.holdAccount, model: "",
                mode: .fullAccess, prompt: "Hold.")
            #expect(await eventually { fleet.sessions[key]?.state == .running && fleet.sessions[key]?.turn != nil })
            await fleet.interrupt(key)
            #expect(await eventually { fleet.lists.projects.first?.state == .done })
            #expect(fleet.lists.projects.flatMap(\.sessions).first { $0.key == key }?.state == .done)
            fleet.suspend()
        }
        // Reopened, as on an app launch: still done until shown.
        guard case .opened(let fleet) = Profile.open(at: profile, client: "test") else {
            Issue.record("cannot reopen the profile")
            return
        }
        #expect(fleet.done.contains(key))
        fleet.watch(key)
        #expect(!fleet.done.contains(key))
        fleet.unwatch(key)
    }
}
