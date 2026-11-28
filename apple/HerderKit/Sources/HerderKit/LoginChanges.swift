import Foundation
import Herder

/// What renaming or removing a login from the Providers list changes, for this device's user:
/// one of a hub's logins changes on the hub, once for every host, and only for the hub's owners;
/// a machine's own login changes on each machine the user owns.
enum LoginTarget: Equatable {
    /// One of the hub's logins, by its name on the hub.
    case hubLogin(hub: HostId, name: String)
    /// The login's accounts on the machines the user owns.
    case accounts([AccountMachine])

    /// What the user may change of `login` over `machines`; nil when they own none of it.
    init?(_ login: ProviderAccounts.Login, machines: [Machine]) {
        if let name = login.hubLogin {
            guard let hub = login.hub, hub.owner else { return nil }
            self = .hubLogin(hub: hub.hostId, name: name)
        } else {
            let rows = AccountMachine.rows(login, machines: machines, summaries: []).filter(\.removable)
            guard !rows.isEmpty else { return nil }
            self = .accounts(rows)
        }
    }

    /// Whether it changes on the hub, for every host at once.
    var onHub: Bool {
        if case .hubLogin = self { true } else { false }
    }

    /// Whether the login's label is the user's to change: the hub's proxy is named for what it
    /// is, so has none shown.
    static func renamable(_ login: ProviderAccounts.Login) -> Bool { !login.isHubProxy }

    /// What removing it asks first, the login shown as `title`; `row` is the machine chosen
    /// when the login is on several.
    func removalQuestion(title: String, row: AccountMachine? = nil) -> (title: String, message: String) {
        switch self {
        case .hubLogin:
            return ("Remove \(title) from the hub?",
                    "The hub’s CLIProxyAPI drops the login and its tokens. Sessions on it move to its other logins.")
        case .accounts(let rows):
            if let row = row ?? (rows.count == 1 ? rows.first : nil) { return row.removalQuestion(title: title) }
            return ("Remove \(title) from which machine?",
                    "Archive its sessions there first. The login stays in its config directory, so adding it again finds it signed in.")
        }
    }
}

/// Renames and removes logins from the Providers list, through `send`; each returns the
/// refusal to show, nil once done.
@MainActor struct LoginChanges {
    typealias Send = @MainActor (HostId, CommandBody) async throws -> CommandResult

    let send: Send

    init(send: @escaping Send) {
        self.send = send
    }

    init(fleet: Fleet) {
        let client = fleet.client
        self.init { try await client.send(hostId: $0, command: $1) }
    }

    /// Removes one of the hub's logins, as its Logins tab's Remove does; or the login's account
    /// from `row`'s machine.
    func remove(_ target: LoginTarget, row: AccountMachine? = nil) async -> String? {
        switch target {
        case .hubLogin(let hub, let name):
            let logins = HubLogins(hub: hub) { try await send(hub, $0) }
            do {
                try await logins.remove(named: name)
                return nil
            } catch {
                return describe(error)
            }
        case .accounts(let rows):
            guard let row = row ?? (rows.count == 1 ? rows.first : nil) else { return nil }
            return await row.remove(through: send)
        }
    }

    /// Names the login `label` on the hub, or on each of its machines the user owns and reaches.
    /// An empty label gives one of the hub's logins back its email; a machine's login keeps its
    /// label, as it needs one.
    func rename(_ target: LoginTarget, to label: String) async -> String? {
        let label = label.trimmingCharacters(in: .whitespacesAndNewlines)
        switch target {
        case .hubLogin(let hub, let name):
            do {
                _ = try await send(hub, .setProviderLogin(name: name, label: label, fallback: nil))
                return nil
            } catch {
                return describe(error)
            }
        case .accounts(let rows):
            guard !label.isEmpty else { return nil }
            var refusals: [String] = []
            for row in rows where row.place.connected {
                guard let account = row.account, account.label != label else { continue }
                do {
                    _ = try await send(row.place.hostId, account.settings(label: label, configDir: account.configDir,
                                                                          fallback: nil))
                } catch {
                    refusals.append("\(row.place.name): \(describe(error))")
                }
            }
            return refusals.isEmpty ? nil : refusals.joined(separator: "\n")
        }
    }
}
