import Foundation
import Herder
import Observation

/// Who is on a machine: its users, each with their role and paired devices. Everyone sees the
/// list; owners change a role, revoke a device and invite someone with a one-time code. Each
/// change goes to the machine, then the list is asked again.
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

    /// A one-time code minted for someone new: the link that pairs them, and when it stops
    /// working.
    struct Invite: Equatable {
        let name: String
        let role: Role
        let link: String
        let expiresAt: Date?
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

    func setRole(_ role: Role, of user: UserInfo) async throws {
        _ = try await send(.setUserRole(userId: user.userId, role: role))
        await reload()
    }

    func revoke(_ device: UserDevice) async throws {
        _ = try await send(.revokeDevice(deviceId: device.deviceId, assertion: nil))
        await reload()
    }

    /// Mints a code that pairs a device as `name`, created as `role`.
    func invite(_ name: String, as role: Role) async throws -> Invite {
        let name = name.trimmingCharacters(in: .whitespaces)
        if name.isEmpty { throw HerderError.Local(detail: "Name who the code is for.") }
        guard case .devicePairing(let code, let fingerprint, let addresses, let expiresAt) =
            try await send(.pairDevice(user: name, role: role))
        else {
            throw HerderError.Local(detail: "the machine did not mint a code")
        }
        let link = pairingUriToString(uri: PairingUri(hosts: addresses, fingerprint: fingerprint, code: code))
        await reload()
        return Invite(name: name, role: role, link: link, expiresAt: Timestamp.date(expiresAt))
    }

    /// The users a person pairs as: all but the hosts that only back up to this vault, which
    /// its page lists as hosts.
    nonisolated static func people(_ users: [UserInfo]) -> [UserInfo] {
        users.filter { $0.devices.isEmpty || !$0.devices.allSatisfy(\.hostOnly) }
    }

    /// Whether `user` is the only owner, whom the machine refuses to make a member.
    nonisolated static func isLastOwner(_ user: UserInfo, in users: [UserInfo]) -> Bool {
        user.role == .owner && users.filter { $0.role == .owner }.count == 1
    }

    /// What a device is, from the client it paired with: "herder-ios/0.6.0" is "iPhone or iPad".
    nonisolated static func deviceName(_ client: String) -> String {
        switch client.split(separator: "/").first {
        case "herder-ios": "iPhone or iPad"
        case "herder-macos": "Mac"
        case "herder-tui": "Terminal"
        case "herder-cli": "Command line"
        case "herder": "Host"
        default: client
        }
    }

    /// "Online · v0.6.0 · paired 3 Oct", or "Last seen 2h ago · …".
    nonisolated static func detail(_ device: UserDevice, now: Date) -> String {
        let version = client(device.client).map { "v\($0)" }
        let seen: String = if device.online {
            "Online"
        } else if let last = device.lastSeen.flatMap(Timestamp.date) {
            Timestamp.age(last, now: now) == "now" ? "Last seen just now" : "Last seen \(Timestamp.age(last, now: now)) ago"
        } else {
            "Not seen yet"
        }
        let paired = Timestamp.date(device.pairedAt).map { "paired \($0.formatted(.dateTime.day().month()))" }
        return [seen, version, paired].compactMap(\.self).joined(separator: " · ")
    }

    /// The version in "herder-ios/0.6.0".
    private nonisolated static func client(_ client: String) -> String? {
        let parts = client.split(separator: "/", maxSplits: 1)
        return parts.count == 2 ? String(parts[1]) : nil
    }
}
