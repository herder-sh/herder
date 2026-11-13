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

    /// A machine sends a journal this device holds no copy of from its start, a page at a time,
    /// and a page can end inside a turn: the turns that ended before this device followed the
    /// session are history in every page. A launch against a fleet nobody was working in
    /// marked nearly every idle session done, and wrote the marks' file each time one changed.
    @Test func theTurnsOfAReplayedJournalAreNoNews() async throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        fleet.isScrolling = { false }
        var script = Script(key.sessionId)
        var events: [EventBody] = [
            created(), .sessionStatusChanged(status: .running, retryAt: nil), .turnStarted(turnId: "t1"),
            .turnCompleted(turnId: "t1", usage: nil), .sessionStatusChanged(status: .idle, retryAt: nil),
        ]
        let journal = events.map { script.event($0) }
        // The machine lists the session with the five events it holds.
        var listed = machine(key.hostId, name: "a", sessions: [key.sessionId])
        listed.sessions[0].headSeq = 5
        fleet.update([listed])
        // Nothing of it is cached, so its first update is empty.
        fleet.applyForTesting(SessionUpdate(events: [], streaming: []), to: key)
        #expect(await fleet.receive(SessionUpdate(events: Array(journal[..<3]), streaming: []), for: key))
        #expect(fleet.session(key)?.status == .running)
        #expect(await fleet.receive(SessionUpdate(events: Array(journal[3...]), streaming: []), for: key))
        #expect(fleet.session(key)?.status == .idle)
        #expect(!fleet.done.contains(key))

        // A turn that ends past what was listed ended while this device followed: it is done.
        events = [
            .sessionStatusChanged(status: .running, retryAt: nil), .turnStarted(turnId: "t2"),
            .turnCompleted(turnId: "t2", usage: nil), .sessionStatusChanged(status: .idle, retryAt: nil),
        ]
        let turn = events.map { script.event($0) }
        #expect(await fleet.receive(SessionUpdate(events: Array(turn[..<2]), streaming: []), for: key))
        #expect(await fleet.receive(SessionUpdate(events: Array(turn[2...]), streaming: []), for: key))
        #expect(fleet.done.contains(key))
    }

    /// The history of a session marked done passes through the turns it ran: the mark stays
    /// through them when the session stands idle at their end, and goes when it does not.
    @Test func aReplayedJournalKeepsTheMarkOfASessionStillIdle() async throws {
        let profile = temporaryProfile()
        let other = SessionKey(hostId: key.hostId, sessionId: "01B")
        var saved = DoneSessions(file: profile.appendingPathComponent("cache/done.json"))
        for key in [key, other] {
            saved.observe(key, before: .running, after: .idle, wasLoaded: true, open: false)
        }
        guard case .opened(let fleet) = Profile.open(at: profile, client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        fleet.isScrolling = { false }
        var listed = machine(key.hostId, name: "a", sessions: [key.sessionId, other.sessionId])
        listed.sessions[0].headSeq = 5
        listed.sessions[1].headSeq = 3
        fleet.update([listed])
        for (key, events) in [(key, 5), (other, 3)] {
            var script = Script(key.sessionId)
            // The second of them was left running: its next turn began after it was marked.
            let journal = [
                created(), .sessionStatusChanged(status: .running, retryAt: nil), .turnStarted(turnId: "t1"),
                .turnCompleted(turnId: "t1", usage: nil), .sessionStatusChanged(status: .idle, retryAt: nil),
            ].prefix(events).map { script.event($0) }
            fleet.applyForTesting(SessionUpdate(events: [], streaming: []), to: key)
            #expect(await fleet.receive(SessionUpdate(events: Array(journal[..<2]), streaming: []), for: key))
            #expect(fleet.done.contains(key))
            #expect(await fleet.receive(SessionUpdate(events: Array(journal[2...]), streaming: []), for: key))
        }
        #expect(fleet.done.contains(key))
        #expect(!fleet.done.contains(other))
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
            let machine = try await fleet.signIn(to: daemon)
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
