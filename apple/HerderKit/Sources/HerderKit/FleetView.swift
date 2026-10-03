import Herder
import SwiftUI

/// The paired machines in a sidebar, and the selected machine beside them.
struct FleetView: View {
    let fleet: Fleet
    @State private var selection: HostId?
    @State private var pairing = false

    var body: some View {
        NavigationSplitView {
            MachineList(fleet: fleet, selection: $selection, pairing: $pairing)
                .navigationSplitViewColumnWidth(min: 240, ideal: 300)
        } detail: {
            if let machine = fleet.machines.first(where: { $0.hostId == selection }) {
                MachineDetail(machine: machine)
            } else {
                ContentUnavailableView("Select a machine", systemImage: "server.rack")
            }
        }
        .sheet(isPresented: $pairing) {
            PairSheet(fleet: fleet)
        }
        .task { await fleet.follow() }
    }
}

struct MachineList: View {
    let fleet: Fleet
    @Binding var selection: HostId?
    @Binding var pairing: Bool

    var body: some View {
        List(fleet.machines, id: \.hostId, selection: $selection) { machine in
            NavigationLink(value: machine.hostId) {
                MachineRow(machine: machine)
            }
        }
        .overlay {
            if fleet.machines.isEmpty {
                ContentUnavailableView {
                    Label("No machines", systemImage: "server.rack")
                } description: {
                    Text("Run `herder pair` on a machine, then add it here with the link it prints.")
                } actions: {
                    Button("Add Machine") { pairing = true }
                        .buttonStyle(.borderedProminent)
                }
            }
        }
        .refreshable { fleet.wake() }
        .navigationTitle("Machines")
        .toolbar {
            ToolbarItemGroup {
                Button("Reconnect", systemImage: "arrow.clockwise") { fleet.wake() }
                    .keyboardShortcut("r")
                Button("Add Machine", systemImage: "plus") { pairing = true }
            }
        }
    }
}

struct MachineRow: View {
    let machine: Machine

    var body: some View {
        Label {
            VStack(alignment: .leading, spacing: 2) {
                Text(machine.name)
                    .font(.headline)
                Text(machine.connection.label)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
        } icon: {
            ConnectionMark(state: machine.connection)
        }
        .padding(.vertical, 4)
    }
}

/// A machine's connection state as a coloured mark.
struct ConnectionMark: View {
    let state: ConnectionState

    var body: some View {
        Image(systemName: state.symbol)
            .foregroundStyle(state.color)
            .accessibilityLabel(state.label)
    }
}

/// Stands in for the machine's sessions until they arrive in P7.2.
struct MachineDetail: View {
    let machine: Machine

    var body: some View {
        ContentUnavailableView(
            machine.name, systemImage: "server.rack",
            description: Text(sessionCount))
            .navigationTitle(machine.name)
    }

    private var sessionCount: String {
        machine.sessions.count == 1 ? "1 session" : "\(machine.sessions.count) sessions"
    }
}

extension ConnectionState {
    var label: String {
        switch self {
        case .connected: "Connected"
        case .connecting: "Connecting…"
        case .disconnected(let error): error
        }
    }

    var symbol: String {
        switch self {
        case .connected: "circle.fill"
        case .connecting: "circle.dotted"
        case .disconnected: "xmark.circle.fill"
        }
    }

    var color: Color {
        switch self {
        case .connected: .green
        case .connecting: .orange
        case .disconnected: .red
        }
    }
}
