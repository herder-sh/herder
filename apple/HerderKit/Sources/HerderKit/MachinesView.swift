import Herder
import SwiftUI

/// The machines: connection, load, each account's usage, and a vault's hosts.
struct MachinesView: View {
    let fleet: Fleet
    @Binding var pairing: Bool
    @State private var renaming: MachineSummary?
    @State private var forgetting: MachineSummary?
    @State private var error: String?

    var body: some View {
        ScrollView {
            LazyVStack(spacing: 12) {
                ForEach(fleet.lists.machines) { machine in
                    MachineCard(machine: machine)
                        .contextMenu {
                            Button("Rename…", systemImage: "pencil") { renaming = machine }
                            Button("Forget", systemImage: "trash", role: .destructive) { forgetting = machine }
                        }
                }
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Theme.failure)
                }
            }
            .frame(maxWidth: 760)
            .frame(maxWidth: .infinity)
            .padding(.horizontal, 16)
            .padding(.bottom, 24)
        }
        .background(Theme.background)
        .refreshable { fleet.wake() }
        .navigationTitle("Machines")
        .toolbar {
            #if os(iOS)
            Button("Reconnect", systemImage: "arrow.clockwise") { fleet.wake() }
            Button("Add Machine", systemImage: "plus") { pairing = true }
            #endif
        }
        .alert("Rename \(renaming?.name ?? "")", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            RenameField(machine: renaming) { name in
                guard let machine = renaming else { return }
                perform { try fleet.rename(machine.hostId, to: name) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("The new name shows on this device only.")
        }
        .confirmationDialog(
            "Forget \(forgetting?.name ?? "") on this device?",
            isPresented: Binding(get: { forgetting != nil }, set: { if !$0 { forgetting = nil } }),
            titleVisibility: .visible
        ) {
            Button("Forget", role: .destructive) {
                guard let machine = forgetting else { return }
                perform { try fleet.forget(machine.hostId) }
            }
        } message: {
            Text("Pair again to get it back.")
        }
    }

    private func perform(_ action: () throws -> Void) {
        do {
            try action()
            error = nil
        } catch {
            self.error = describe(error)
        }
    }
}

private struct RenameField: View {
    let machine: MachineSummary?
    let rename: (String) -> Void
    @State private var name = ""

    var body: some View {
        TextField("Name", text: $name)
            .onAppear { name = machine?.name ?? "" }
        Button("Rename") {
            let trimmed = name.trimmingCharacters(in: .whitespaces)
            if !trimmed.isEmpty { rename(trimmed) }
        }
    }
}

struct MachineCard: View {
    let machine: MachineSummary

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 14) {
                HStack(spacing: 10) {
                    Image(systemName: machine.hosts.isEmpty ? "server.rack" : "archivebox")
                        .font(.title3)
                        .foregroundStyle(machine.connected ? Theme.text : Theme.tertiary)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(machine.name).font(.headline).foregroundStyle(Theme.text)
                        HStack(spacing: 5) {
                            ConnectionMark(state: machine.connection)
                            Text(machine.connection.label).lineLimit(2)
                            if machine.role == .owner { Text("· Owner") }
                            if machine.role == .member { Text("· Member") }
                            if !machine.hosts.isEmpty { Text("· Vault") }
                        }
                        .font(.caption)
                        .foregroundStyle(Theme.secondary)
                    }
                    Spacer()
                    VStack(alignment: .trailing, spacing: 2) {
                        if machine.running > 0 {
                            Text("\(machine.running) running").foregroundStyle(Theme.running)
                        }
                        Text(machine.sessions == 1 ? "1 session" : "\(machine.sessions) sessions")
                            .foregroundStyle(Theme.tertiary)
                    }
                    .font(.caption.weight(.medium))
                }
                if let cpu = machine.cpu, let memory = machine.memory {
                    HStack(spacing: 16) {
                        Meter(label: "CPU", percent: cpu)
                        Meter(label: "Memory", percent: memory)
                    }
                }
                ForEach(machine.hosts) { host in
                    HStack(spacing: 8) {
                        Circle().fill(host.online ? Theme.success : Theme.failure).frame(width: 6, height: 6)
                        Text(host.name).foregroundStyle(Theme.text)
                        if !host.online {
                            Text("offline · \(host.lastSeen) ago").foregroundStyle(Theme.failure)
                        }
                        Spacer()
                        Text("\(host.sessions)").foregroundStyle(Theme.tertiary)
                    }
                    .font(.subheadline)
                }
                ForEach(machine.accounts) { AccountUsage(account: $0) }
                if machine.connected && machine.accounts.isEmpty && machine.hosts.isEmpty {
                    Text("No accounts yet").font(.footnote).foregroundStyle(Theme.tertiary)
                }
                if machine.pinned {
                    Label("Failover pinned: sessions stay on their account", systemImage: "pin")
                        .font(.caption)
                        .foregroundStyle(Theme.tertiary)
                }
            }
        }
        .opacity(machine.connected ? 1 : 0.75)
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
                Circle().fill(Theme.success).frame(width: 7, height: 7)
            case .connecting:
                Circle().strokeBorder(Theme.waiting, style: StrokeStyle(lineWidth: 1.5, dash: [2, 2]))
                    .frame(width: 9, height: 9)
            case .disconnected:
                Circle().fill(Theme.failure).frame(width: 10, height: 10)
                    .overlay(Image(systemName: "xmark").font(.system(size: 5, weight: .heavy))
                        .foregroundStyle(Theme.background))
            }
        }
        .accessibilityLabel(state.label)
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

private struct Meter: View {
    let label: String
    let percent: Double

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(label).foregroundStyle(Theme.secondary)
                Spacer()
                Text("\(Int(percent.rounded()))%").foregroundStyle(Theme.text).monospacedDigit()
            }
            .font(.caption.weight(.medium))
            UsageBar(percent: percent, height: 5)
        }
    }
}

private struct AccountUsage: View {
    let account: AccountSummary

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Text(account.label).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                Text(account.provider).font(.caption).foregroundStyle(Theme.secondary)
                if account.failover { Chip(text: "failover") }
                Spacer()
                Text(account.sessions == 1 ? "1 session" : "\(account.sessions) sessions")
                    .font(.caption)
                    .foregroundStyle(Theme.tertiary)
            }
            if account.usage.isEmpty {
                Text("No usage reported yet").font(.caption).foregroundStyle(Theme.tertiary)
            }
            ForEach(account.usage) { window in
                HStack(spacing: 10) {
                    Text(window.label).lineLimit(1).frame(width: 96, alignment: .leading).foregroundStyle(Theme.secondary)
                    UsageBar(percent: window.percent)
                    Text("\(Int(window.percent.rounded()))%").frame(width: 36, alignment: .trailing)
                        .foregroundStyle(Theme.text)
                    Text(window.resets).frame(width: 56, alignment: .trailing).foregroundStyle(Theme.tertiary)
                }
                .font(.caption.monospacedDigit())
            }
        }
        .padding(12)
        .background(Theme.raised.opacity(0.6), in: .rect(cornerRadius: Theme.corner))
    }
}
