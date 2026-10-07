import Foundation
import Herder

/// A session's state as the user sees it: the TUI's statuses, with a pending approval or
/// question folded into `needsYou`, and an idle session this device has not opened since its
/// turn finished `done` (`DoneSessions`).
enum SessionState: Hashable {
    case running, waiting, needsYou, idle, done, error, archived, moved

    var label: String {
        switch self {
        case .running: "Running"
        case .waiting: "Waiting for capacity"
        case .needsYou: "Needs you"
        case .idle: "Idle"
        case .done: "Done"
        case .error: "Error"
        case .archived: "Archived"
        case .moved: "Moved"
        }
    }

    /// Whether the machine takes a new title: archived and moved sessions are read-only.
    var renamable: Bool { self != .archived && self != .moved }
}

/// One row of a session list.
struct SessionSummary: Hashable, Identifiable {
    var id: SessionKey { key }
    let key: SessionKey
    let title: String
    let project: String
    let branch: String
    var worktree = ""
    /// The machine it runs on: the machine's name, or for a vault the host's.
    let machine: String
    var machineOffline = false
    let state: SessionState
    let activity: String
    let age: String
    var prs: [PullRequest] = []
    var depth = 0
    var children = 0
    var childrenNeedYou = 0
    /// The provider's own agents to list under it (`NativeAgent.listed`).
    var agents: [NativeAgent] = []
}

/// An approval or a question waiting on the user.
struct PendingRequest: Hashable, Identifiable {
    enum Kind: Hashable {
        case approval(summary: String)
        case question(text: String, choices: [String])
    }

    var id: String { "\(session.key.hostId)/\(session.key.sessionId)/\(requestId)" }
    /// The approval or question id.
    let requestId: String
    let session: SessionSummary
    let kind: Kind
    let since: Date
    let age: String
    var reason: String?
    var note: String?
}

struct UsageWindowSummary: Hashable, Identifiable {
    var id: String { label }
    let label: String
    let percent: Double
    let resets: String
}

struct AccountSummary: Hashable, Identifiable {
    var id: String { accountId }
    let accountId: String
    let label: String
    let provider: String
    let sessions: Int
    let usage: [UsageWindowSummary]
}

struct FleetHostSummary: Hashable, Identifiable {
    var id: String { name }
    let name: String
    let online: Bool
    let lastSeen: String
    let sessions: Int
}

struct MachineSummary: Hashable, Identifiable {
    var id: HostId { hostId }
    let hostId: HostId
    let name: String
    let connection: ConnectionState
    let role: Role?
    /// Percentages, once the daemon has reported its resources.
    let cpu: Double?
    let memory: Double?
    let running: Int
    /// The host's turns against its limit, once it has reported them; a vault has none.
    var turns: TurnLoad?
    let sessions: Int
    let accounts: [AccountSummary]
    let hosts: [FleetHostSummary]
    let pinned: Bool

    var connected: Bool { connection == .connected }
}
