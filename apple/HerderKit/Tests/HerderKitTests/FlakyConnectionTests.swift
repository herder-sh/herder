import Foundation
import Herder
@testable import HerderKit
import Testing

/// The app on a connection that drops and comes back: `devbox` runs session `01A`, and a hub
/// replicates it.
@MainActor
struct FlakyConnectionTests {
    private let direct = SessionKey(hostId: "devbox", sessionId: "01A")
    private let replica = SessionKey(hostId: "hub", sessionId: "01A")

    private func openFleet() throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CocoaError(.fileNoSuchFile)
        }
        return fleet
    }

    private func devbox(_ connection: ConnectionState = .connected, sessions: [SessionId] = ["01A"]) -> Machine {
        machine("devbox", name: "devbox", sessions: sessions, connection: connection)
    }

    private func hub(_ connection: ConnectionState = .connected) -> Machine {
        var hub = machine("hub", name: "hub", sessions: ["01A"], hosts: [
            FleetHost(hostId: "devbox", hostName: "devbox", online: true, lastSeen: ""),
        ], connection: connection)
        hub.sessions[0].hostId = "devbox"
        return hub
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

    // MARK: A long drop

    /// The host stays away for longer than the client rides out: the lists show the hub's copy, and neither the
    /// row nor the open session loses anything while that copy loads, nor when the host is back.
    @Test func aLongDropKeepsTheOpenSessionAndItsRow() async throws {
        let fleet = try openFleet()
        list([devbox(), hub()], on: fleet)
        fleet.applyForTesting(SessionUpdate(events: journal(), streaming: []), to: direct)
        list([devbox(), hub()], on: fleet)
        fleet.watch(direct)
        // What this device sent and the machine has yet to take stays too.
        await fleet.submit("And the changelog", to: direct)
        let before = try #require(rows(fleet).first)
        let transcript = fleet.transcript(direct).map(\.id)
        #expect(!transcript.isEmpty)

        // The host is gone: the hub's copy is listed, as the host's showed until now.
        list([devbox(.disconnected(error: "unreachable")), hub()], on: fleet)
        var row = try #require(rows(fleet).first)
        #expect(rows(fleet).count == 1)
        #expect(row.key == replica)
        #expect(row.title == before.title)
        #expect(row.state == before.state)
        #expect(row.updatedAt == before.updatedAt)
        #expect(fleet.listed(direct) == replica)
        // The open session still shows the host's copy: the hub's has not loaded.
        #expect(fleet.source(for: direct) == direct)
        #expect(fleet.transcript(direct).map(\.id) == transcript)
        #expect(fleet.session(direct)?.outbox.map(\.text) == ["And the changelog"])

        // The hub's copy loads, and has two more messages: the open session moves on to it.
        fleet.applyForTesting(SessionUpdate(events: journal(more: 2), streaming: []), to: replica)
        list([devbox(.disconnected(error: "unreachable")), hub()], on: fleet)
        #expect(fleet.source(for: direct) == replica)
        #expect(fleet.session(replica)?.lastSeq == 6)
        #expect(fleet.session(direct) != nil)

        // The host is back: its copy is listed again, showing what the hub's got to meanwhile
        // until its own journal has replayed.
        list([devbox(), hub()], on: fleet)
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
        list([devbox(), hub()], on: fleet)
        #expect(fleet.session(replica) == nil)
        #expect(fleet.session(direct)?.loaded == true)
    }

    /// A message sent from the host's copy shows on whichever copy the open session moves to,
    /// as the connections drop and come back, until the copy shown has journaled it.
    @Test func aSentMessageMovesWithTheCopyShown() async throws {
        let fleet = try openFleet()
        list([devbox(), hub()], on: fleet)
        fleet.applyForTesting(SessionUpdate(events: journal(), streaming: []), to: direct)
        fleet.watch(direct)
        await fleet.submit("Tidy the README", to: direct)
        let shows = { (key: SessionKey) in
            fleet.transcript(key).contains { block in
                if case .user(_, "Tidy the README", _, .some, _, _) = block { true } else { false }
            }
        }
        #expect(shows(direct))

        // The host drops, and the hub's copy, further along, is shown: the message too, though
        // the hub's journal has the same words from before it was sent.
        list([devbox(.disconnected(error: "unreachable")), hub()], on: fleet)
        fleet.applyForTesting(SessionUpdate(events: journal(more: 2), streaming: []), to: replica)
        #expect(fleet.source(for: direct) == replica)
        #expect(shows(replica))

        // The hub's copy journals it: it shows from the journal there.
        let taken = Event(sessionId: "01A", seq: 7, at: "2026-01-01T00:00:07Z", by: nil, body: .itemAdded(item: Item(
            agentMessage: nil, parentCallId: nil, id: "u2", turnId: "t2",
            body: .userMessage(text: "Tidy the README", attachments: []))))
        fleet.applyForTesting(SessionUpdate(events: [taken], streaming: []), to: replica)
        #expect(!shows(replica))
        #expect(fleet.session(replica)?.outbox.isEmpty == true)
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
        let online = Lists.copies([devbox(), hub()])
        #expect(online.shown[origin] == direct)
        #expect(online.shadowed == [replica])
        #expect(online.standIns == [replica: direct])

        // The network is gone: neither copy is the better one, so the one listed stays.
        let down = ConnectionState.disconnected(error: "offline")
        #expect(Lists.copies([devbox(down), hub(down)], keeping: online.shown).shown[origin] == direct)
        let away = Lists.copies([devbox(down), hub()], keeping: online.shown)
        #expect(away.shown[origin] == replica)
        #expect(Lists.copies([devbox(down), hub(down)], keeping: away.shown).shown[origin] == replica)
        // Back on its host as soon as that connects.
        #expect(Lists.copies([devbox(), hub(down)], keeping: away.shown).shown[origin] == direct)
    }

    @Test func aSessionTwoHubsReplicateIsListedOnce() {
        var second = hub()
        second.hostId = "hub-2"
        let copies = Lists.copies([devbox(.connecting), hub(), second])
        #expect(copies.shown[direct] == replica)
        #expect(copies.shadowed == [direct, SessionKey(hostId: "hub-2", sessionId: "01A")])
        let listed = Lists(machines: [devbox(.connecting), hub(), second], sessions: [:]).projects.flatMap(\.sessions)
        #expect(listed.map(\.key) == [replica])
    }

    // MARK: The connections shown

    /// The client lists a machine as connected through a short drop, and as disconnected, then
    /// connecting, with each retry of a long one: the app shows it offline once, until it is
    /// back, and nothing it lists moves meanwhile.
    @Test func aMachineAwayShowsOfflineThroughItsRetries() throws {
        let fleet = try openFleet()
        list([devbox(), hub()], on: fleet)
        fleet.applyForTesting(SessionUpdate(events: journal(), streaming: []), to: direct)
        list([devbox(), hub()], on: fleet)
        fleet.watch(direct)
        let transcript = fleet.transcript(direct).map(\.id)
        #expect(!transcript.isEmpty)

        let down = ConnectionState.disconnected(error: "reset")
        list([devbox(down), hub()], on: fleet)
        let (rowsAway, machinesAway) = (rows(fleet), fleet.lists.machines)
        #expect(rowsAway.map(\.key) == [replica])
        for connection in [.connecting, down, .connecting, down, .connecting] as [ConnectionState] {
            list([devbox(connection), hub()], on: fleet)
            #expect(fleet.machines.map(\.connection) == [down, .connected])
            #expect(rows(fleet) == rowsAway)
            #expect(fleet.lists.machines == machinesAway)
            #expect(fleet.transcript(direct).map(\.id) == transcript)
        }
        // The latest failure is the one shown.
        let unreachable = ConnectionState.disconnected(error: "unreachable")
        list([devbox(unreachable), hub()], on: fleet)
        #expect(fleet.machines.first?.connection == unreachable)
        list([devbox(.connecting), hub()], on: fleet)
        #expect(fleet.machines.first?.connection == unreachable)

        list([devbox(), hub()], on: fleet)
        #expect(fleet.machines.map(\.connection) == [.connected, .connected])
        #expect(rows(fleet).map(\.key) == [direct])
        #expect(fleet.transcript(direct).map(\.id) == transcript)
    }

    /// Only a machine shown offline keeps its mark: a first attempt shows, and so does the
    /// attempt under way when the client stops riding out a drop.
    @Test func anAttemptShowsUnlessTheMachineShowsOffline() throws {
        let fleet = try openFleet()
        list([devbox(.connecting)], on: fleet)
        #expect(fleet.machines.first?.connection == .connecting)
        list([devbox()], on: fleet)
        list([devbox(.connecting)], on: fleet)
        #expect(fleet.machines.first?.connection == .connecting)
        let down = ConnectionState.disconnected(error: "unreachable")
        list([devbox(down)], on: fleet)
        list([devbox(.connecting)], on: fleet)
        #expect(fleet.machines.first?.connection == down)
    }

    /// The client pings a connected machine every 10 s and tells of each answer: once it has
    /// told of nothing for 30 s in the foreground, its machines no longer read connected, as a
    /// phone's did for hours after both its connections were gone. What it tells next shows.
    @Test func aMachineTheClientTellsNothingOfStopsReadingConnected() throws {
        let fleet = try openFleet()
        list([devbox()], on: fleet)
        fleet.wake()
        fleet.checkHeard(now: .now + .seconds(20))
        #expect(fleet.machines.first?.connection == .connected)
        fleet.checkHeard(now: .now + Fleet.silence)
        #expect(fleet.machines.first?.connection == .disconnected(error: "no answer for 30 s"))
        list([devbox()], on: fleet)
        #expect(fleet.machines.first?.connection == .connected)

        // In the background iOS freezes the app, and nothing is heard: no news is no sign.
        fleet.suspend()
        fleet.checkHeard(now: .now + .seconds(600))
        #expect(fleet.machines.first?.connection == .connected)
        // Back in the foreground, the client gets its time to tell of the connections again.
        fleet.wake()
        fleet.checkHeard(now: .now + .seconds(20))
        #expect(fleet.machines.first?.connection == .connected)
    }

    /// While this device has no connection to the host, the session shows from the hub's
    /// copy: the hub refuses a prompt, so the session is read-only, and its row says the
    /// host is not connected though the hub sees it online.
    @Test func aSessionShownFromTheHubSaysItsHostIsNotConnected() throws {
        let fleet = try openFleet()
        list([devbox(), hub()], on: fleet)
        #expect(rows(fleet).first?.fromHub == false)
        #expect(!fleet.isReplica(direct))

        list([devbox(.disconnected(error: "unreachable")), hub()], on: fleet)
        let row = try #require(rows(fleet).first)
        #expect(row.key == replica)
        #expect(row.fromHub)
        #expect(!row.machineOffline)
        #expect(fleet.isReplica(replica))
        #expect(!fleet.isReplica(direct))
    }
}
