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
            Group {
                if let machine = fleet.machines.first(where: { $0.hostId == selection }) {
                    MachineDetail(machine: machine)
                } else {
                    ContentUnavailableView("Select a machine", systemImage: "server.rack")
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Theme.background)
        }
        .tint(Theme.text)
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
            .listRowBackground(Theme.surface)
        }
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .overlay {
            if fleet.machines.isEmpty {
                ContentUnavailableView {
                    Label("No machines", systemImage: "server.rack")
                } description: {
                    Text("Run `herder pair` on a machine, then add it here with the link it prints.")
                } actions: {
                    Button("Add Machine") { pairing = true }
                        .font(.body.weight(.semibold))
                        .foregroundStyle(Theme.onPrimary)
                        .padding(.horizontal, 20)
                        .frame(minHeight: 44)
                        .background(Theme.primary, in: .rect(cornerRadius: Theme.corner))
                        .buttonStyle(.plain)
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
                    .foregroundStyle(Theme.text)
                Text(machine.connection.label)
                    .font(.subheadline)
                    .foregroundStyle(Theme.secondary)
                    .lineLimit(2)
            }
        } icon: {
            ConnectionMark(state: machine.connection)
        }
        .padding(.vertical, 4)
    }
}

/// A machine's connection state as a mark: filled when connected, a dashed ring while
/// connecting, a crossed circle when the connection failed.
struct ConnectionMark: View {
    let state: ConnectionState

    var body: some View {
        Group {
            switch state {
            case .connected:
                Circle().fill(Theme.success).frame(width: 10, height: 10)
            case .connecting:
                Circle().strokeBorder(Theme.waiting, style: StrokeStyle(lineWidth: 2, dash: [2, 2]))
                    .frame(width: 12, height: 12)
            case .disconnected:
                Circle().fill(Theme.failure).frame(width: 14, height: 14)
                    .overlay(Image(systemName: "xmark").font(.system(size: 7, weight: .heavy))
                        .foregroundStyle(Theme.background))
            }
        }
        .frame(width: 20, height: 20)
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
}
