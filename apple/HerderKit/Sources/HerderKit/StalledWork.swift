import Herder
import SwiftUI

/// The one thing the chrome says about the machines: that open work waits on one that is
/// offline. Nothing while all is well; a machine asleep with nothing open, one still
/// connecting, or a hub, say nothing either. It opens the machine's page in Hub, or Hub when it
/// names more than one. At the foot of the sidebar, as a mark alone on the rail, and at the foot
/// of the iPhone's Board.
struct StalledWork: View {
    let machines: [MachineSummary]
    /// The mark alone, the line in its tooltip, for the rail.
    var compact = false
    /// Opens the machine's page, or Hub for `nil`.
    let open: (HostId?) -> Void

    /// The machines that run sessions, are offline, and had work open on them.
    nonisolated static func stalled(_ machines: [MachineSummary]) -> [MachineSummary] {
        machines.filter { machine in
            guard !machine.hub, machine.openWork > 0, case .disconnected = machine.connection else { return false }
            return true
        }
    }

    /// "devbox is offline · 2 sessions wait on it", or `nil` when no open work waits.
    nonisolated static func line(_ machines: [MachineSummary]) -> String? {
        let stalled = stalled(machines)
        guard let first = stalled.first else { return nil }
        let work = stalled.map(\.openWork).reduce(0, +)
        let sessions = work == 1 ? "1 session waits" : "\(work) sessions wait"
        return stalled.count == 1
            ? "\(first.name) is offline · \(sessions) on it"
            : "\(stalled.count) machines are offline · \(sessions) on them"
    }

    var body: some View {
        let stalled = Self.stalled(machines)
        if let first = stalled.first, let line = Self.line(machines) {
            Button { open(stalled.count == 1 ? first.hostId : nil) } label: {
                Group {
                    if compact {
                        ConnectionMark(state: first.connection)
                            .frame(width: 38, height: 28)
                    } else {
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            ConnectionMark(state: first.connection)
                            Text(line).fixedSize(horizontal: false, vertical: true)
                            Spacer(minLength: 0)
                        }
                        .font(.footnote)
                        .foregroundStyle(Theme.secondary)
                        .padding(.horizontal, Sidebar.inset)
                        .padding(.vertical, 8)
                    }
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .help(line)
            .accessibilityLabel(line)
            .accessibilityIdentifier("stalled-work")
            .padding(.vertical, compact ? 0 : 4)
        }
    }
}

/// The stalled-work line at the foot of the iPhone's Board: a view of its own, so only it draws
/// again as the machines change.
struct BoardStalledWork: View {
    let fleet: Fleet
    let open: (HostId?) -> Void

    var body: some View {
        StalledWork(machines: fleet.lists.machines, open: open)
            .padding(.horizontal, 8)
            .background(.ground)
    }
}
