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
    /// Each install's CLI version when it started, as `Fleet.cliVersion` reads it.
    private var before: [ProviderJobKey: String] = [:]
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

    /// Starts installing or updating `provider`'s CLI on `hostId`, from `version`, unless that
    /// runs already. Use `Fleet.install`, which also opens its terminal.
    @discardableResult
    mutating func install(_ provider: Provider, on hostId: HostId, from version: String?) -> ProviderJobKey {
        let key = ProviderJobKey(hostId: hostId, job: .install(provider))
        if connections[key].map(Self.running) != true {
            put(key, TerminalConnection(hostId: hostId, terminalId: nil, install: provider))
            before[key] = version
        }
        return key
    }

    /// The CLI version an install started from; `nil` when it was not installed.
    func before(_ key: ProviderJobKey) -> String? { before[key] }

    /// Forgets a terminal, once it has ended and its outcome has been seen.
    mutating func clear(_ key: ProviderJobKey) {
        connections[key] = nil
        before[key] = nil
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

extension Fleet {
    /// Starts installing or updating `provider`'s CLI on `hostId`, unless that runs already, and
    /// opens its terminal at once: a job runs whether its terminal is on screen or not, so
    /// Update All runs every one. The emulator takes its own size once it is shown.
    @discardableResult
    func install(_ provider: Provider, on hostId: HostId) -> ProviderJobKey {
        let key = providerTerminals.install(provider, on: hostId, from: cliVersion(provider, on: hostId))
        providerTerminals[key]?.connect(client: client, sessionId: nil, cols: 80, rows: 24)
        return key
    }

    /// The version of `provider`'s CLI `hostId` lists, as its number: empty when the machine
    /// cannot read it, `nil` when it is not installed.
    func cliVersion(_ provider: Provider, on hostId: HostId) -> String? {
        let status = machines.first { $0.hostId == hostId }?.providers.first { $0.provider == provider }
        guard let status, status.installed else { return nil }
        return status.version.map(ProviderVersions.number) ?? ""
    }

    /// Where the install `key` stands, from its terminal and the CLI version before and now.
    func installProgress(_ key: ProviderJobKey) -> JobProgress {
        guard case .install(let provider) = key.job, let connection = providerTerminals[key] else { return .starting }
        return JobProgress(install: connection.state, before: providerTerminals.before(key),
                           after: cliVersion(provider, on: key.hostId))
    }

    /// Cancels the login `key` runs: closes its terminal on the machine, which hangs up the
    /// provider's login, so it adds no account. Its terminal then shows it ended.
    func cancelLogin(_ key: ProviderJobKey) async throws {
        guard let terminalId = providerTerminals[key]?.terminalId else {
            throw HerderError.Local(detail: "the login has no terminal on the machine yet")
        }
        try await client.closeTerminal(hostId: key.hostId, terminalId: terminalId)
    }
}

/// Where a login or install stands, as its row and header say it.
enum JobProgress: Equatable {
    /// Opening the terminal on the machine.
    case starting
    /// The login runs, waiting on the user to finish the provider's own login.
    case running
    /// The installer or updater runs; it needs no one.
    case installing
    /// The login ended and the machine lists the account.
    case added
    /// The login's terminal ended without the account showing up.
    case ended
    /// The install ended, and the machine lists the CLI at this version.
    case installed(String)
    /// The update ended, and the machine lists the CLI at a new version.
    case updated(from: String, to: String)
    /// The update ended, and the machine lists the CLI at the version it had.
    case upToDate(String)
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

    /// An install's: `before` is the CLI's version when it started, `after` the one the machine
    /// lists now, each `nil` when not installed. The daemon checks the CLI again before it
    /// closes the terminal, so `after` is current once the terminal has exited.
    init(install state: TerminalConnection.State, before: String?, after: String?) {
        switch (state, before, after) {
        case (.connecting, _, _): self = .starting
        case (.attached, _, _): self = .installing
        case (.exited(0), _, nil): self = .failed("The installer finished, but the machine does not find the CLI.")
        case (.exited(0), nil, let after?): self = .installed(after)
        case (.exited(0), let before?, let after?):
            self = before == after ? .upToDate(after) : .updated(from: before, to: after)
        case (.exited(let code?), _, _): self = .failed("The installer exited with \(code).")
        case (.exited(nil), _, _): self = .failed("The installer was stopped.")
        case (.failed(let message), _, _): self = .failed(message)
        }
    }

    var finished: Bool {
        switch self {
        case .starting, .running, .installing: false
        case .added, .ended, .installed, .updated, .upToDate, .failed: true
        }
    }
}
