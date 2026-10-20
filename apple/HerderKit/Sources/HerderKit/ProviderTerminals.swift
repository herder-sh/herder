import Herder

/// What a provider terminal runs on a machine: an account's login, to add it or to log it in
/// again, or a provider CLI's installer or updater.
enum ProviderJob: Hashable {
    case add(AccountId)
    case relogin(AccountId)
    case install(Provider)
}

/// A provider terminal: the machine it runs on and what it runs there. Each has its own
/// terminal, so logins and installs on one machine, for any providers, run side by side.
struct ProviderJobKey: Hashable, Identifiable {
    let hostId: HostId
    let job: ProviderJob

    var id: Self { self }
}

/// The logins and installs this device started, kept while the app runs so a sheet closed in
/// the middle of one comes back to it. Each stays until the user clears it once it has ended.
@MainActor
struct ProviderTerminals {
    private var connections: [ProviderJobKey: TerminalConnection] = [:]
    /// The keys oldest first, so the lists keep their order.
    private(set) var keys: [ProviderJobKey] = []

    subscript(key: ProviderJobKey) -> TerminalConnection? { connections[key] }

    /// Starts adding `account` on `hostId`. The id is the account's: one id can be added only
    /// once at a time, so a login for it already here is returned instead.
    @discardableResult
    mutating func add(_ account: NewAccount, on hostId: HostId) -> ProviderJobKey {
        let key = ProviderJobKey(hostId: hostId, job: .add(account.accountId))
        if connections[key].map(Self.running) != true {
            put(key, TerminalConnection(hostId: hostId, terminalId: nil, account: account))
        }
        return key
    }

    /// Starts logging `accountId` in again on `hostId`, unless its login runs already.
    @discardableResult
    mutating func relogin(_ accountId: AccountId, on hostId: HostId) -> ProviderJobKey {
        let key = ProviderJobKey(hostId: hostId, job: .relogin(accountId))
        if connections[key].map(Self.running) != true {
            put(key, TerminalConnection(hostId: hostId, terminalId: nil, relogin: accountId))
        }
        return key
    }

    /// Starts installing or updating `provider`'s CLI on `hostId`, unless that runs already.
    @discardableResult
    mutating func install(_ provider: Provider, on hostId: HostId) -> ProviderJobKey {
        let key = ProviderJobKey(hostId: hostId, job: .install(provider))
        if connections[key].map(Self.running) != true {
            put(key, TerminalConnection(hostId: hostId, terminalId: nil, install: provider))
        }
        return key
    }

    /// Forgets a terminal, once it has ended and its outcome has been seen.
    mutating func clear(_ key: ProviderJobKey) {
        connections[key] = nil
        keys.removeAll { $0 == key }
    }

    /// The accounts being added on `hostId`, oldest first, running or ended.
    func adds(on hostId: HostId) -> [ProviderJobKey] {
        keys.filter { key in
            guard key.hostId == hostId, case .add = key.job else { return false }
            return true
        }
    }

    /// The installs and updates on every machine, oldest first.
    var installs: [ProviderJobKey] {
        keys.filter { key in
            guard case .install = key.job else { return false }
            return true
        }
    }

    /// The account ids the logins still running on `hostId` hold: a new account cannot take
    /// one, as the daemon also refuses.
    func pendingIds(on hostId: HostId) -> [AccountId] {
        adds(on: hostId).compactMap { key in
            guard case .add(let id) = key.job, connections[key].map(Self.running) == true else { return nil }
            return id
        }
    }

    /// Whether `connection` has not ended yet.
    static func running(_ connection: TerminalConnection) -> Bool {
        switch connection.state {
        case .connecting, .attached: true
        case .exited, .failed: false
        }
    }

    private mutating func put(_ key: ProviderJobKey, _ connection: TerminalConnection) {
        connections[key] = connection
        keys.removeAll { $0 == key }
        keys.append(key)
    }
}

/// Where a login or install stands, as its row and header say it.
enum JobProgress: Equatable {
    /// Opening the terminal on the machine.
    case starting
    /// Running: waiting on the user to finish the provider's own login, or installing.
    case running
    /// The login ended and the machine lists the account.
    case added
    /// The terminal ended; for a login, without the account showing up.
    case ended
    case failed(String)

    /// `added` tells whether the machine lists the account the login adds.
    init(_ state: TerminalConnection.State, added: Bool) {
        switch state {
        case .connecting: self = added ? .added : .starting
        case .attached: self = added ? .added : .running
        case .exited: self = added ? .added : .ended
        case .failed(let message): self = .failed(message)
        }
    }

    var finished: Bool {
        switch self {
        case .starting, .running: false
        case .added, .ended, .failed: true
        }
    }
}
