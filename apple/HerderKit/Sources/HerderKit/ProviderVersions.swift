import Foundation
import Herder

/// Each provider CLI's version on each machine that runs sessions, against the newest any of
/// them has: what is behind, what is missing, and what herder can install or update there.
struct ProviderVersions: Equatable {
    /// A provider's CLI on one machine.
    struct Cell: Equatable, Identifiable {
        enum State: Equatable {
            /// Installed, as new as any machine has.
            case current
            /// Installed, older than another machine's.
            case outdated
            /// Installed, with a version herder cannot read.
            case unknown
            case missing
        }

        let hostId: HostId
        let machine: String
        let provider: Provider
        /// The version the CLI printed, trimmed to its number where it has one.
        let version: String?
        let state: State
        /// What herder can run for it now, if anything.
        let action: Action?
        /// Why the action cannot run, when there is one to run and it cannot.
        let blocked: String?

        var id: HostId { hostId }
    }

    enum Action: Equatable {
        case install, update

        var title: String { self == .install ? "Install" : "Update" }
    }

    struct Row: Equatable, Identifiable {
        let provider: Provider
        /// The newest version any machine has.
        let latest: String?
        let cells: [Cell]

        var id: Provider { provider }
        var outdated: [Cell] { cells.filter { $0.state == .outdated } }
        var missing: [Cell] { cells.filter { $0.state == .missing } }
        /// The machines herder can bring up to date now.
        var updatable: [Cell] { outdated.filter { $0.action == .update && $0.blocked == nil } }
    }

    let rows: [Row]

    /// The machines that run sessions, connected or not: a vault runs none.
    init(machines all: [Machine]) {
        let machines = all.filter { $0.hosts.isEmpty && $0.role != nil }
        let reported = Set(machines.flatMap(\.providers).filter(\.installed).map(\.provider))
        let used = Set(machines.flatMap(\.accounts).map(\.provider))
        let providers = ModelCatalog.knownProviders.filter { reported.contains($0) || used.contains($0) }
            + reported.union(used).subtracting(ModelCatalog.knownProviders).sorted()
        rows = providers.map { provider in
            let statuses = machines.compactMap { machine in machine.providers.first { $0.provider == provider } }
            // The newest version a machine reports; the daemon's own latest-version check, once
            // it reports one, belongs here.
            let latest = statuses.compactMap(\.version).map(Self.number)
                .max { Self.version($1, isNewerThan: $0) }
            return Row(provider: provider, latest: latest, cells: machines.map { machine in
                Self.cell(provider, on: machine, latest: latest)
            })
        }
    }

    /// Everything herder can update now, over every provider.
    var updatable: [Cell] { rows.flatMap(\.updatable) }

    private static func cell(_ provider: Provider, on machine: Machine, latest: String?) -> Cell {
        let status = machine.providers.first { $0.provider == provider }
        let version = status?.version.map(number)
        let state: Cell.State
        if status?.installed != true {
            state = .missing
        } else if let version, !version.isEmpty, let latest {
            state = self.version(latest, isNewerThan: version) ? .outdated : .current
        } else {
            state = .unknown
        }
        let action: Action? = switch state {
        case .missing: .install
        case .outdated: .update
        case .current, .unknown: nil
        }
        let blocked: String? = switch action {
        case nil: nil
        case _ where machine.role != .owner: "Only the machine’s owner can install CLIs."
        case _ where machine.connection != .connected: "\(machine.name) is not connected."
        case .install where status?.canInstall != true:
            "herder has no installer for \(ModelCatalog.providerName(provider)) on \(machine.name)’s OS."
        case .update where status?.canUpdate != true:
            "herder has no updater for \(ModelCatalog.providerName(provider)) on \(machine.name)’s OS."
        default: nil
        }
        return Cell(hostId: machine.hostId, machine: machine.name, provider: provider, version: version,
                    state: state, action: action, blocked: blocked)
    }

    /// The version's number, `2.1.14` of `2.1.14 (Claude Code)`; the line itself without one.
    static func number(_ line: String) -> String {
        let start = line.firstIndex(where: \.isNumber) ?? line.endIndex
        let digits = line[start...].prefix { $0.isNumber || $0 == "." }
        let trimmed = digits.trimmingCharacters(in: CharacterSet(charactersIn: "."))
        return trimmed.contains(".") ? trimmed : line.trimmingCharacters(in: .whitespaces)
    }

    static func version(_ left: String, isNewerThan right: String) -> Bool {
        // Swift arrays are not Comparable; match Rust Vec lexicographic order.
        parts(right).lexicographicallyPrecedes(parts(left))
    }

    private static func parts(_ text: String) -> [Int] {
        let start = text.firstIndex(where: \.isNumber) ?? text.endIndex
        let digits = text[start...].prefix { $0.isNumber || $0 == "." }
        return digits.split(separator: ".").compactMap { Int($0) }
    }
}
