import Foundation
import Herder

/// The machines' connections as the app shows them. A connection that drops still shows as
/// connected for `grace`: on a flaky network it is back within moments, and nothing the app
/// shows by it (a machine's mark, which copy of a session is listed, what a page reloads once
/// connected) should change twice for that. A command sent meanwhile waits for the connection,
/// as any does. Past the grace the drop shows, and stays "offline" through the retries that
/// follow, rather than turn to "connecting" and back with each.
struct SteadyConnections {
    /// How long a dropped connection still shows as connected.
    var grace = Duration.seconds(5)
    /// While the app is in the background no drop shows: iOS freezes it there, and a grace
    /// that ran out meanwhile would show every machine offline as the app comes back, just
    /// before each reconnects.
    private var held = false
    private var shown: [HostId: ConnectionState] = [:]
    /// When each machine shown as connected was first seen not to be.
    private var dropped: [HostId: ContinuousClock.Instant] = [:]

    /// `machines` with the connections to show at `now`.
    mutating func steadied(_ machines: [Machine], now: ContinuousClock.Instant = .now) -> [Machine] {
        let listed = Set(machines.map(\.hostId))
        shown = shown.filter { listed.contains($0.key) }
        dropped = dropped.filter { listed.contains($0.key) }
        return machines.map { machine in
            var machine = machine
            machine.connection = show(machine.connection, of: machine.hostId, now: now)
            return machine
        }
    }

    private mutating func show(_ real: ConnectionState, of hostId: HostId, now: ContinuousClock.Instant) -> ConnectionState {
        if real == .connected {
            dropped[hostId] = nil
            shown[hostId] = .connected
            return .connected
        }
        let previous = shown[hostId]
        if previous == .connected {
            let since = dropped[hostId] ?? now
            dropped[hostId] = since
            if held || now - since < grace { return .connected }
            dropped[hostId] = nil
        } else if real == .connecting, let previous, case .disconnected = previous {
            return previous
        }
        shown[hostId] = real
        return real
    }

    /// When the next drop in its grace shows, unless its machine is back by then.
    var expires: ContinuousClock.Instant? {
        held ? nil : dropped.values.min().map { $0 + grace }
    }

    /// The app went to the background: see `held`.
    mutating func hold() { held = true }

    /// The app is back, or the user asked to reconnect: each dropped connection gets the whole
    /// grace again to come back in, and a machine shown offline shows the attempt.
    mutating func restart(now: ContinuousClock.Instant = .now) {
        held = false
        for hostId in dropped.keys { dropped[hostId] = now }
        shown = shown.filter { if case .disconnected = $0.value { false } else { true } }
    }
}
