import Herder
import SwiftUI

/// Where a project is across the machines: the remote its clones have, and the machines with no
/// clone of it yet. Only a project with a remote is cloned onto another machine; a vault holds
/// no clones, and a machine never connected has not said what it holds.
struct ProjectReach: Equatable {
    struct Missing: Equatable, Identifiable {
        let hostId: HostId
        let name: String
        let connected: Bool
        let owner: Bool

        var id: HostId { hostId }
    }

    /// The `origin` remote of the project's first clone that has one.
    let remote: String?
    let missing: [Missing]

    init(projectId: ProjectId, machines: [Machine]) {
        remote = machines.lazy.flatMap(\.projects).first { $0.projectId == projectId && $0.remote != nil }?.remote
        missing = remote == nil ? [] : machines
            .filter { $0.hosts.isEmpty && $0.role != nil && !$0.projects.contains { $0.projectId == projectId } }
            .map { Missing(hostId: $0.hostId, name: $0.name, connected: $0.connection == .connected, owner: $0.role == .owner) }
    }
}

/// The machines a project is missing on, each with a way to clone it there, into the machine's
/// projects folder.
struct ProjectReachGroup: View {
    let fleet: Fleet
    let reach: ProjectReach
    /// The machines a clone is running on.
    @State private var cloning: Set<HostId> = []
    @State private var error: String?

    var body: some View {
        if let remote = reach.remote, !reach.missing.isEmpty {
            SettingsGroup(title: "Missing on") {
                ForEach(Array(reach.missing.enumerated()), id: \.element.id) { index, machine in
                    if index > 0 { RowDivider() }
                    SettingRow(label: machine.name, detail: detail(machine, remote: remote)) {
                        if cloning.contains(machine.hostId) {
                            ProgressView().controlSize(.small)
                        } else {
                            let enabled = machine.owner && machine.connected
                            Button("Clone") { Task { await clone(remote, on: machine.hostId) } }
                                .buttonStyle(.plain)
                                .font(.subheadline.weight(.semibold))
                                .foregroundStyle(Theme.text)
                                .padding(.horizontal, 12)
                                .frame(height: 32)
                                .background(Theme.raised, in: .rect(cornerRadius: 7))
                                .disabled(!enabled)
                                .opacity(enabled ? 1 : 0.4)
                        }
                    }
                }
            }
            if let error {
                Label(error, systemImage: "exclamationmark.triangle").font(.footnote).foregroundStyle(Theme.failure)
            }
        }
    }

    private func detail(_ machine: ProjectReach.Missing, remote: String) -> String {
        if cloning.contains(machine.hostId) { return "Cloning \(remote)…" }
        if !machine.owner { return "Only its owners can clone it there" }
        if !machine.connected { return "Connect to it to clone it there" }
        return "Clones \(remote) into its projects folder"
    }

    private func clone(_ remote: String, on hostId: HostId) async {
        cloning.insert(hostId)
        defer { cloning.remove(hostId) }
        do {
            _ = try await fleet.cloneProject(remote, into: nil, on: hostId)
            error = nil
        } catch {
            self.error = describe(error)
        }
    }
}
