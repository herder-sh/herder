import Foundation
import Herder
import Observation

/// A secret's value as the user types it. It is never shown, printed nor mirrored: its
/// descriptions and its mirror hide it, so a dump of a form that holds one leaks nothing.
struct SecretInput: Equatable, CustomStringConvertible, CustomDebugStringConvertible, CustomReflectable {
    var text = ""

    var isEmpty: Bool { text.isEmpty }
    var description: String { "‹hidden›" }
    var debugDescription: String { "‹hidden›" }
    var customMirror: Mirror { Mirror(self, children: []) }
}

/// A vault's secrets, for its owners: its sets with the hosts each is granted to, the secrets in
/// each, and its latest fetches. Each change goes to the vault, then the lists are asked again.
/// No answer carries a value back, so replacing a secret is the only way to change one.
@MainActor @Observable
final class VaultSecrets {
    typealias Send = @MainActor (CommandBody) async throws -> CommandResult

    /// A list the vault was asked for.
    enum Load<T> {
        case asking
        case loaded(T)
        /// The vault's refusal, as it said it.
        case failed(String)
    }

    let vault: HostId
    private let send: Send
    private(set) var list: Load<(sets: [SecretSet], secrets: [SecretInfo])> = .asking
    /// Newest first.
    private(set) var audit: Load<[SecretAccess]> = .asking

    init(vault: HostId, send: @escaping Send) {
        self.vault = vault
        self.send = send
    }

    /// The secrets of `vault`, sent through `fleet`.
    convenience init(fleet: Fleet, vault: HostId) {
        let client = fleet.client
        self.init(vault: vault) { try await client.send(hostId: vault, command: $0) }
    }

    var sets: [SecretSet] {
        if case .loaded(let list) = list { list.sets } else { [] }
    }

    func secrets(in set: String) -> [SecretInfo] {
        guard case .loaded(let list) = list else { return [] }
        return list.secrets.filter { $0.set == set }
    }

    /// Asks the vault for its sets and secrets, and its latest fetches.
    func reload() async {
        do {
            guard case .secrets(let sets, let secrets) = try await send(.listSecrets) else {
                throw HerderError.Local(detail: "the vault did not list its secrets")
            }
            list = .loaded((sets, secrets))
        } catch {
            list = .failed(describe(error))
        }
        do {
            guard case .secretAudit(let entries) = try await send(.listSecretAudit(before: nil)) else {
                throw HerderError.Local(detail: "the vault did not list its secrets' fetches")
            }
            audit = .loaded(entries)
        } catch {
            audit = .failed(describe(error))
        }
    }

    /// Creates the set `name`.
    func createSet(_ name: String) async throws {
        let name = name.trimmingCharacters(in: .whitespaces)
        if let problem = Self.setNameProblem(name) { throw HerderError.Local(detail: problem) }
        try await change(.createSecretSet(set: name))
    }

    func deleteSet(_ set: String) async throws {
        try await change(.deleteSecretSet(set: set))
    }

    func deleteSecret(_ secret: SecretInfo) async throws {
        try await change(.deleteSecret(set: secret.set, name: secret.name))
    }

    /// Grants `set` to `host`, or revokes it.
    func setGranted(_ granted: Bool, set: String, host: HostId) async throws {
        try await change(granted ? .grantSecretSet(set: set, hostId: host) : .revokeSecretSet(set: set, hostId: host))
    }

    /// Sends `command`, then asks for the lists again.
    fileprivate func change(_ command: CommandBody) async throws {
        _ = try await send(command)
        await reload()
    }

    /// The hosts a set can be granted to, each with its name if the vault lists it: the vault's
    /// hosts, then any the set is granted to that the vault no longer lists.
    static func hosts(of set: SecretSet?, vaultHosts: [FleetHost]) -> [(id: HostId, name: String?)] {
        var hosts: [(id: HostId, name: String?)] = vaultHosts.map { ($0.hostId, $0.hostName) }
        for granted in set?.hosts ?? [] where !hosts.contains(where: { $0.id == granted }) {
            hosts.append((granted, nil))
        }
        return hosts
    }

    /// Why `name` cannot name a set, as the vault checks it: lowercase ASCII letters, digits and
    /// single hyphens inside, at most 64.
    static func setNameProblem(_ name: String) -> String? {
        let valid = (1...64).contains(name.count)
            && name.allSatisfy { ("a"..."z").contains($0) || ("0"..."9").contains($0) || $0 == "-" }
            && !name.hasPrefix("-") && !name.hasSuffix("-") && !name.contains("--")
        return valid ? nil : "A set's name is lowercase letters, digits and single hyphens, such as prod or coingate-prod."
    }

    /// Whether `name` may name a secret, as an environment variable: uppercase ASCII letters,
    /// digits and underscores, not starting with a digit, at most 64.
    static func isValidSecretName(_ name: String) -> Bool {
        guard let first = name.first, !first.isNumber else { return false }
        return name.count <= 64
            && name.allSatisfy { ("A"..."Z").contains($0) || ("0"..."9").contains($0) || $0 == "_" }
    }
}

/// Adding a secret to a set, or replacing one: its name, description and value. Saving takes the
/// value out of the form before the command goes, so it is gone whether the vault keeps it or
/// refuses it, and the user types it again.
@MainActor @Observable
final class SecretEditor {
    let secrets: VaultSecrets
    let set: String
    /// Replacing a secret, whose name is then fixed; else adding one.
    let replacing: Bool
    var name: String
    var description: String
    var value = SecretInput()
    private(set) var error: String?
    private(set) var sending = false

    /// A new secret in `set`.
    init(secrets: VaultSecrets, set: String) {
        self.secrets = secrets
        self.set = set
        replacing = false
        name = ""
        description = ""
    }

    /// Replaces `secret`, keeping its description until changed.
    init(secrets: VaultSecrets, replacing secret: SecretInfo) {
        self.secrets = secrets
        set = secret.set
        replacing = true
        name = secret.name
        description = secret.description
    }

    /// What is wrong with the form, or `nil` when it can be sent.
    var problem: String? {
        let description = description.trimmingCharacters(in: .whitespacesAndNewlines)
        if !VaultSecrets.isValidSecretName(name.trimmingCharacters(in: .whitespaces)) {
            return "A secret's name is an environment variable's: A-Z, 0-9 and _, such as API_KEY."
        }
        if description.isEmpty { return "Say what the secret is for: agents read it to pick one." }
        if description.count > 200 || description.contains(where: \.isNewline) {
            return "The description is one line of at most 200 characters."
        }
        if value.isEmpty { return "Enter the value." }
        if value.text.utf8.count > 64 * 1024 || value.text.contains("\0") {
            return "The value is longer than 64 KiB, or holds a NUL."
        }
        return nil
    }

    /// Sends the secret; whether the vault kept it. The value is cleared first either way.
    func save() async -> Bool {
        if let problem {
            error = problem
            if !value.isEmpty && problem.hasPrefix("The value") { value = SecretInput() }
            return false
        }
        let command = CommandBody.putSecret(
            set: set, name: name.trimmingCharacters(in: .whitespaces),
            description: description.trimmingCharacters(in: .whitespacesAndNewlines), value: value.text)
        value = SecretInput()
        error = nil
        sending = true
        defer { sending = false }
        do {
            try await secrets.change(command)
            return true
        } catch {
            self.error = describe(error)
            return false
        }
    }
}
