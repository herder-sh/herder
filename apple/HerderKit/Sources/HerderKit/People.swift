import Foundation
import Herder
import Observation

/// Who is on a machine: its users, each with their role and paired devices, for the pages that
/// name them (a bot's people, who a session acts for). The People page lists people instead.
@MainActor @Observable
final class People {
    typealias Send = @MainActor (CommandBody) async throws -> CommandResult

    let hostId: HostId
    private let send: Send
    /// The people, as `people(_:)` picks them from the machine's users.
    private(set) var users: Load<[UserInfo]> = .asking

    init(hostId: HostId, send: @escaping Send) {
        self.hostId = hostId
        self.send = send
    }

    /// The people of `hostId`, sent through `fleet`.
    convenience init(fleet: Fleet, hostId: HostId) {
        let client = fleet.client
        self.init(hostId: hostId) { try await client.send(hostId: hostId, command: $0) }
    }

    func reload() async {
        do {
            guard case .users(let users) = try await send(.listUsers) else {
                throw HerderError.Local(detail: "the machine did not list its users")
            }
            self.users = .loaded(Self.people(users))
        } catch {
            users = .failed(describe(error))
        }
    }

    /// The users a person pairs as: all but the hosts that only back up to this vault, which
    /// its page lists as hosts.
    nonisolated static func people(_ users: [UserInfo]) -> [UserInfo] {
        users.filter { $0.devices.isEmpty || !$0.devices.allSatisfy(\.hostOnly) }
    }
}
