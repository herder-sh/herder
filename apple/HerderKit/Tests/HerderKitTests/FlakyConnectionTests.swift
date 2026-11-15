import Foundation
import Herder
@testable import HerderKit
import Testing

/// The app on a connection that drops and comes back: `devbox` runs session `01A`, and a vault
/// replicates it.
@MainActor
struct FlakyConnectionTests {
    private let direct = SessionKey(hostId: "devbox", sessionId: "01A")
    private let replica = SessionKey(hostId: "vault", sessionId: "01A")

    private func openFleet() throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CocoaError(.fileNoSuchFile)
        }
        return fleet
    }

    private func devbox(_ connection: ConnectionState = .connected, sessions: [SessionId] = ["01A"]) -> Machine {
        machine("devbox", name: "devbox", sessions: sessions, connection: connection)
    }

    private func vault(_ connection: ConnectionState = .connected) -> Machine {
        var vault = machine("vault", name: "vault", sessions: ["01A"], hosts: [
            FleetHost(hostId: "devbox", hostName: "devbox", online: true, lastSeen: ""),
        ], connection: connection)
        vault.sessions[0].hostId = "devbox"
        return vault
    }

    /// Takes `machines` as the client would list them, and shows the lists at once.
    private func list(_ machines: [Machine], on fleet: Fleet) {
        fleet.update(machines)
        fleet.setMachinesForTesting(machines)
    }

    /// The session's journal: created, prompted, answered, then `more` answers.
    private func journal(more: Int = 0) -> [Event] {
        var script = Script("01A", host: "devbox")
        func item(_ id: String, _ body: ItemBody) -> EventBody {
            .itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: id, turnId: "t1", body: body))
        }
        var bodies: [EventBody] = [
            created(task: "Tidy the README"), .turnStarted(turnId: "t1"),
            item("u1", .userMessage(text: "Tidy the README", attachments: [])),
            item("a1", .assistantMessage(text: "On it.")),
        ]
        for index in 0..<more { bodies.append(item("m\(index)", .assistantMessage(text: "Step \(index)."))) }
        return bodies.map { script.event($0) }
    }

    private func rows(_ fleet: Fleet) -> [SessionSummary] { fleet.lists.projects.flatMap(\.sessions) }

    // MARK: A short drop

    @Test func aShortDropChangesNothingOnScreen() throws {
        let fleet = try openFleet()
        list([devbox(), vault()], on: fleet)
        fleet.applyForTesting(SessionUpdate(events: journal(), streaming: []), to: direct)
        list([devbox(), vault()], on: fleet)
        fleet.watch(direct)
        let (rowsBefore, machinesBefore) = (rows(fleet), fleet.lists.machines)
        let transcript = fleet.transcript(direct).map(\.id)
        #expect(rowsBefore.map(\.key) == [direct])
        #expect(!transcript.isEmpty)

        // The network goes and comes back twice, each time within the grace.
        for connection in [.disconnected(error: "reset"), .connecting, .connected, .connecting, .connected] as [ConnectionState] {
            list([devbox(connection), vault(connection)], on: fleet)
            #expect(fleet.machines.map(\.connection) == [.connected, .connected])
            #expect(rows(fleet) == rowsBefore)
            #expect(fleet.lists.machines == machinesBefore)
            #expect(fleet.source(for: direct) == direct)
            #expect(fleet.session(direct)?.loaded == true)
            #expect(fleet.transcript(direct).map(\.id) == transcript)
        }
    }

    /// Only the session's host drops, as when it restarts: the vault's copy does not take its
    /// place for that.
    @Test func aHostsShortDropDoesNotMoveItsSessionsToTheVault() throws {
        let fleet = try openFleet()
        list([devbox(), vault()], on: fleet)
        fleet.applyForTesting(SessionUpdate(events: journal(), streaming: []), to: direct)
        list([devbox(.disconnected(error: "reset")), vault()], on: fleet)
        #expect(rows(fleet).map(\.key) == [direct])
        #expect(fleet.session(direct)?.loaded == true)
        list([devbox(), vault()], on: fleet)
        #expect(rows(fleet).map(\.key) == [direct])
    }

    @Test func aDropShowsOnceItsGraceIsOver() async throws {
        let fleet = try openFleet()
        fleet.steady.grace = .milliseconds(100)
        list([devbox(), vault()], on: fleet)
        list([devbox(.disconnected(error: "reset")), vault()], on: fleet)
        #expect(fleet.machines.map(\.connection) == [.connected, .connected])
        #expect(await eventually { fleet.machines.first?.connection == .disconnected(error: "reset") })
        #expect(fleet.lists.machines.first?.connection == .disconnected(error: "reset"))
    }

    // MARK: A long drop

    /// The host stays away past the grace: the lists show the vault's copy, and neither the
    /// row nor the open session loses anything while that copy loads, nor when the host is back.
    @Test func aLongDropKeepsTheOpenSessionAndItsRow() async throws {
        let fleet = try openFleet()
        fleet.steady.grace = .zero
        list([devbox(), vault()], on: fleet)
        fleet.applyForTesting(SessionUpdate(events: journal(), streaming: []), to: direct)
        list([devbox(), vault()], on: fleet)
        fleet.watch(direct)
        // What this device sent and the machine has yet to take stays too.
        await fleet.submit("And the changelog", to: direct)
        let before = try #require(rows(fleet).first)
        let transcript = fleet.transcript(direct).map(\.id)
        #expect(!transcript.isEmpty)

        // The host is gone: the vault's copy is listed, as the host's showed until now.
        list([devbox(.disconnected(error: "unreachable")), vault()], on: fleet)
        var row = try #require(rows(fleet).first)
        #expect(rows(fleet).count == 1)
        #expect(row.key == replica)
        #expect(row.title == before.title)
        #expect(row.state == before.state)
        #expect(row.updatedAt == before.updatedAt)
        #expect(fleet.listed(direct) == replica)
        // The open session still shows the host's copy: the vault's has not loaded.
        #expect(fleet.source(for: direct) == direct)
        #expect(fleet.transcript(direct).map(\.id) == transcript)
        #expect(fleet.session(direct)?.outbox.map(\.text) == ["And the changelog"])

        // The vault's copy loads, and has two more messages: the open session moves on to it.
        fleet.applyForTesting(SessionUpdate(events: journal(more: 2), streaming: []), to: replica)
        list([devbox(.disconnected(error: "unreachable")), vault()], on: fleet)
        #expect(fleet.source(for: direct) == replica)
        #expect(fleet.session(replica)?.lastSeq == 6)
        #expect(fleet.session(direct) != nil)

        // The host is back: its copy is listed again, showing what the vault's got to meanwhile
        // until its own journal has replayed.
        list([devbox(), vault()], on: fleet)
        row = try #require(rows(fleet).first)
        #expect(rows(fleet).count == 1)
        #expect(row.key == direct)
        #expect(row.updatedAt == fleet.session(replica)?.updatedAt)
        #expect(fleet.source(for: direct) == replica)
        #expect(fleet.transcript(direct).map(\.id) == transcript)

        // The journal replays from its start, in two updates: nothing shows twice, nothing
        // goes away meanwhile, and the prompt not yet taken is still there.
        let events = journal(more: 2)
        #expect(await fleet.receive(SessionUpdate(events: Array(events.prefix(2)), streaming: []), for: direct))
        #expect(fleet.transcript(direct).map(\.id) == transcript)
        #expect(await fleet.receive(SessionUpdate(events: Array(events.dropFirst(2)), streaming: []), for: direct))
        let model = try #require(fleet.session(direct))
        #expect(model.timeline.map(\.seq) == events.map(\.seq))
        #expect(model.outbox.map(\.text) == ["And the changelog"])
        #expect(fleet.source(for: direct) == direct)

        // Closed, the copy that stood in is let go.
        fleet.unwatch(direct)
        list([devbox(), vault()], on: fleet)
        #expect(fleet.session(replica) == nil)
        #expect(fleet.session(direct)?.loaded == true)
    }

    /// A session its machine stops listing while it is open keeps what it shows.
    @Test func anOpenSessionKeepsItsStateWhenItsMachineStopsListingIt() throws {
        let fleet = try openFleet()
        list([devbox()], on: fleet)
        fleet.applyForTesting(SessionUpdate(events: journal(), streaming: []), to: direct)
        fleet.watch(direct)
        list([devbox(sessions: [])], on: fleet)
        #expect(fleet.session(direct)?.loaded == true)
        #expect(!fleet.transcript(direct).isEmpty)
        fleet.unwatch(direct)
        list([devbox(sessions: [])], on: fleet)
        #expect(fleet.session(direct) == nil)
    }

    // MARK: Which copy is listed

    @Test func withNothingConnectedTheCopyListedStays() {
        let origin = direct
        let online = Lists.copies([devbox(), vault()])
        #expect(online.shown[origin] == direct)
        #expect(online.shadowed == [replica])
        #expect(online.standIns == [replica: direct])

        // The network is gone: neither copy is the better one, so the one listed stays.
        let down = ConnectionState.disconnected(error: "offline")
        #expect(Lists.copies([devbox(down), vault(down)], keeping: online.shown).shown[origin] == direct)
        let away = Lists.copies([devbox(down), vault()], keeping: online.shown)
        #expect(away.shown[origin] == replica)
        #expect(Lists.copies([devbox(down), vault(down)], keeping: away.shown).shown[origin] == replica)
        // Back on its host as soon as that connects.
        #expect(Lists.copies([devbox(), vault(down)], keeping: away.shown).shown[origin] == direct)
    }

    @Test func aSessionTwoVaultsReplicateIsListedOnce() {
        var second = vault()
        second.hostId = "vault-2"
        let copies = Lists.copies([devbox(.connecting), vault(), second])
        #expect(copies.shown[direct] == replica)
        #expect(copies.shadowed == [direct, SessionKey(hostId: "vault-2", sessionId: "01A")])
        let listed = Lists(machines: [devbox(.connecting), vault(), second], sessions: [:]).projects.flatMap(\.sessions)
        #expect(listed.map(\.key) == [replica])
    }

    // MARK: The connections shown

    @Test func aDroppedConnectionShowsConnectedForItsGrace() {
        var steady = SteadyConnections()
        let start = ContinuousClock.now
        func shown(_ connection: ConnectionState, after seconds: Int) -> ConnectionState? {
            steady.steadied([devbox(connection)], now: start + .seconds(seconds)).first?.connection
        }
        let down = ConnectionState.disconnected(error: "reset")
        #expect(shown(.connected, after: 0) == .connected)
        #expect(shown(down, after: 1) == .connected)
        #expect(steady.expires == start + .seconds(6))
        #expect(shown(.connecting, after: 4) == .connected)
        #expect(shown(.connected, after: 5) == .connected)
        #expect(steady.expires == nil)

        // A drop that lasts shows once the grace is over, and stays through the retries.
        #expect(shown(down, after: 10) == .connected)
        #expect(shown(down, after: 15) == down)
        #expect(shown(.connecting, after: 16) == down)
        #expect(shown(.connected, after: 17) == .connected)
    }

    @Test func aMachineNeverConnectedShowsAsItIs() {
        var steady = SteadyConnections()
        #expect(steady.steadied([devbox(.connecting)]).first?.connection == .connecting)
        let down = ConnectionState.disconnected(error: "unreachable")
        #expect(steady.steadied([devbox(down)]).first?.connection == down)
        // Reconnect, or the app back in front: the attempt shows.
        steady.restart()
        #expect(steady.steadied([devbox(.connecting)]).first?.connection == .connecting)
    }

    @Test func noDropShowsWhileTheAppIsInTheBackground() {
        var steady = SteadyConnections()
        let start = ContinuousClock.now
        let down = ConnectionState.disconnected(error: "reset")
        _ = steady.steadied([devbox()], now: start)
        steady.hold()
        #expect(steady.steadied([devbox(down)], now: start + .seconds(1)).first?.connection == .connected)
        #expect(steady.steadied([devbox(down)], now: start + .seconds(600)).first?.connection == .connected)
        #expect(steady.expires == nil)
        // Back in front, the connection has the whole grace to return in.
        steady.restart(now: start + .seconds(600))
        #expect(steady.steadied([devbox(down)], now: start + .seconds(604)).first?.connection == .connected)
        #expect(steady.steadied([devbox(down)], now: start + .seconds(605)).first?.connection == down)
    }
}
