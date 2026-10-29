import Foundation
import Herder
import Observation

/// Who is on a machine: its people, each with their role, for the pages that name them (a bot's
/// people, who a session acts for). The People page lists the whole fleet's instead.
@MainActor @Observable
final class People {
    typealias Send = @MainActor (CommandBody) async throws -> CommandResult

    let hostId: HostId
    private let send: Send
    private(set) var people: Load<[PersonInfo]> = .asking

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
            guard case .people(let people) = try await send(.listPeople) else {
                throw HerderError.Local(detail: "the machine did not list its people")
            }
            self.people = .loaded(people)
        } catch {
            people = .failed(describe(error))
        }
    }
}
