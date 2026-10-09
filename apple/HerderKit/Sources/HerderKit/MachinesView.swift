import Herder
import SwiftUI

/// The machines: connection, load, each account's usage, and on compact width the way to Skills,
/// Providers and a vault's hosts.
struct MachinesView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    /// Shows each paired vault under the machines, where it has no section of its own.
    var showsVaults = false
    /// Leads to Skills and Providers, where they have no section of their own.
    var showsSections = false
    @State private var renaming: MachineSummary?
    @State private var forgetting: MachineSummary?
    @State private var error: String?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                if showsSections {
                    skillsLink
                    providersLink
                }
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 360), spacing: 14, alignment: .top)], spacing: 14) {
                    ForEach(fleet.lists.machines) { machine in
                        MachineCard(machine: machine, since: fleet.connectionLog[machine.hostId]?.last?.at) {
                            sheet = .machineSettings(hostId: machine.hostId)
                        }
                            .contextMenu {
                                Button("Settings…", systemImage: "gearshape") { sheet = .machineSettings(hostId: machine.hostId) }
                                Button("Rename…", systemImage: "pencil") { renaming = machine }
                                Button("Forget", systemImage: "trash", role: .destructive) { forgetting = machine }
                            }
                    }
                    if let error {
                        Text(error).font(.footnote).foregroundStyle(Theme.failure)
                    }
                }
                if showsVaults {
                    ForEach(fleet.vaults) { VaultSection(vault: $0) }
                }
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 24)
        }
        .background(Theme.background)
        .refreshable { fleet.wake() }
        .navigationTitle("Machines")
        .toolbar {
            #if os(iOS)
            Button("Reconnect", systemImage: "arrow.clockwise") { fleet.wake() }
            Button("Add Machine", systemImage: "plus") { sheet = .pair }
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

    private var providersLink: some View {
        NavigationLink { ProvidersView(fleet: fleet) } label: {
            Card(padding: 12) {
                HStack(spacing: 10) {
                    Image(systemName: "person.2").foregroundStyle(Theme.secondary)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Providers").font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                        let missing = ProviderAccounts(machines: fleet.machines).groups
                            .flatMap(\.logins).filter { !$0.missing.isEmpty }.count
                        Text(missing == 0 ? "Accounts on each machine"
                             : missing == 1 ? "1 login missing on a machine" : "\(missing) logins missing on a machine")
                            .font(.caption).foregroundStyle(Theme.secondary)
                    }
                    Spacer()
                    Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                }
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("providers-link")
    }

    private var skillsLink: some View {
        NavigationLink { SkillsView(fleet: fleet) } label: {
            Card(padding: 12) {
                HStack(spacing: 10) {
                    Image(systemName: "book.closed").foregroundStyle(Theme.secondary)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Skills").font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                        let count = SkillLibrary(fleet.machines).skills.count
                        Text(count == 1 ? "1 skill in the library" : "\(count) skills in the library")
                            .font(.caption).foregroundStyle(Theme.secondary)
                    }
                    Spacer()
                    Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                }
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("skills-link")
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
    /// When the connection reached its current state, since the app opened.
    var since: Date?
    let settings: () -> Void

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 14) {
                HStack(spacing: 10) {
                    Image(systemName: machine.hosts.isEmpty ? "server.rack" : "archivebox")
                        .font(.title3)
                        .foregroundStyle(machine.connected ? Theme.text : Theme.tertiary)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(machine.name).font(.headline).foregroundStyle(Theme.text)
                        TimelineView(.periodic(from: .now, by: 30)) { context in
                            // One line: the age, then the role, drop before the state would wrap.
                            ViewThatFits(in: .horizontal) {
                                status([age(now: context.date), role, vault])
                                status([role, vault])
                                status([])
                            }
                        }
                        .font(.caption)
                        .foregroundStyle(Theme.secondary)
                    }
                    Spacer()
                    VStack(alignment: .trailing, spacing: 2) {
                        HStack(spacing: 6) {
                            if machine.running > 0 {
                                Text("\(machine.running) running").foregroundStyle(Theme.running)
                            }
                            if let turns = machine.turns {
                                Text(turns.fraction)
                                    .monospacedDigit()
                                    .foregroundStyle(turns.waiting > 0 ? Theme.accent : Theme.tertiary)
                                    .help(turns.usage)
                            }
                        }
                        Text(machine.sessions == 1 ? "1 session" : "\(machine.sessions) sessions")
                            .foregroundStyle(Theme.tertiary)
                    }
                    .font(.caption.weight(.medium))
                    .fixedSize()
                    IconButton(symbol: "gearshape", help: "Machine Settings", action: settings)
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

    private func status(_ details: [String?]) -> some View {
        HStack(spacing: 5) {
            ConnectionMark(state: machine.connection)
            Text(([machine.connection.label] + details.compactMap { $0 }).joined(separator: " · "))
                .lineLimit(1)
        }
    }

    private func age(now: Date) -> String? {
        guard let since else { return nil }
        let age = Timestamp.age(since, now: now)
        return "\(machine.connected ? "for" : "since") \(age == "now" ? "a moment" : age)"
    }

    private var role: String? {
        switch machine.role {
        case .owner: "Owner"
        case .member: "Member"
        default: nil
        }
    }

    private var vault: String? { machine.hosts.isEmpty ? nil : "Vault" }
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
        case .disconnected(let error): Self.explain(error)
        }
    }

    /// A disconnection as the user can act on it: a daemon on another protocol version needs
    /// updating, which the raw error buries among unreachable addresses.
    static func explain(_ error: String) -> String {
        guard let theirsAt = error.range(of: "this daemon speaks "),
              let oursAt = error.range(of: "protocol version ") else { return error }
        let theirs = error[theirsAt.upperBound...].prefix { $0.isNumber }
        let ours = error[oursAt.upperBound...].prefix { $0.isNumber }
        return "Runs a different herder (protocol \(theirs); this app speaks \(ours)). Update one of them to connect."
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
