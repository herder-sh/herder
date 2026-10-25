import Herder

/// Where a project is across the machines: the machines with clones of it, the remote those
/// clones have, and the machines with no clone of it yet. Only a project with a remote is cloned
/// onto another machine; a vault holds no clones, and a machine never connected has not said
/// what it holds.
struct ProjectReach: Equatable {
    /// A machine with the project: where its clones are and how many sessions it has there.
    struct Clone: Equatable, Identifiable {
        let hostId: HostId
        let name: String
        let paths: [String]
        let sessions: Int
        /// Sessions that are not archived, which block removing the project from the machine.
        let live: Int
        let owner: Bool

        var id: HostId { hostId }
    }

    struct Missing: Equatable, Identifiable {
        let hostId: HostId
        let name: String
        let connected: Bool
        let owner: Bool

        var id: HostId { hostId }
    }

    let clones: [Clone]
    /// The `origin` remote of the project's first clone that has one.
    let remote: String?
    let missing: [Missing]

    init(projectId: ProjectId, machines: [Machine]) {
        clones = machines.compactMap { machine in
            guard let project = machine.projects.first(where: { $0.projectId == projectId }) else { return nil }
            let sessions = machine.sessions.filter { $0.projectId == projectId }
            return Clone(hostId: machine.hostId, name: machine.name, paths: project.paths, sessions: sessions.count,
                         live: sessions.filter { $0.status != .archived }.count, owner: machine.role == .owner)
        }
        remote = machines.lazy.flatMap(\.projects).first { $0.projectId == projectId && $0.remote != nil }?.remote
        missing = remote == nil ? [] : machines
            .filter { $0.hosts.isEmpty && $0.role != nil && !$0.projects.contains { $0.projectId == projectId } }
            .map { Missing(hostId: $0.hostId, name: $0.name, connected: $0.connection == .connected, owner: $0.role == .owner) }
    }
}
