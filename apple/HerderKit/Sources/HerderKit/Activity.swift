import Foundation
import Herder

/// What a background job runs on a machine: an account's login, to add it or to log it in
/// again, a provider CLI's installer or updater, an AWS IAM Identity Center sign-in, or setting
/// up a vault next to its daemon.
enum JobKind: Hashable {
    case add(AccountId)
    case relogin(AccountId)
    case install(Provider)
    case awsSignIn(profile: String)
    case vault
}

/// A background job: the machine it runs on and what it runs there. One of each per machine at
/// a time; starting it again while it runs returns the one running.
struct JobKey: Hashable, Identifiable {
    let hostId: HostId
    let kind: JobKind

    var id: Self { self }
}

/// The long-running jobs this device started, kept while the app runs: CLI installs and
/// updates, account logins, AWS sign-ins and vault setups. Each but a vault setup runs in its
/// own terminal on its machine; each stays listed until the user clears it once it has ended.
///
/// Installs queue per machine: one installer at a time on a machine, as two package managers
/// writing the same prefix trip over each other, while different machines run theirs at once.
/// Logins and sign-ins start at once, as they wait on the user and would hold the queue.
@MainActor
struct Activity {
    struct Job {
        /// Its terminal; a vault setup has none.
        let connection: TerminalConnection?
        /// A vault setup's step.
        var vault: VaultSetupStep?
        /// Whether its terminal was opened; until then it is queued.
        var started = false
        /// An install's CLI version when it started, as `Fleet.cliVersion` reads it.
        var before: String?
        /// When its terminal ended.
        var ended: Date?
    }

    /// Where a job stands in the queue.
    enum Phase: Equatable {
        case queued, running, ended
    }

    private var jobs: [JobKey: Job] = [:]
    /// The keys oldest first, so the lists keep their order and the queue its turns.
    private(set) var keys: [JobKey] = []

    subscript(key: JobKey) -> Job? { jobs[key] }

    /// Queues `connection` as `key`, unless that job is queued or runs already; a job that has
    /// ended is replaced, to run again. Returns whether it was queued.
    @discardableResult
    mutating func enqueue(_ key: JobKey, _ connection: TerminalConnection?) -> Bool {
        if let job = jobs[key], job.ended == nil { return false }
        jobs[key] = Job(connection: connection)
        keys.removeAll { $0 == key }
        keys.append(key)
        return true
    }

    mutating func start(_ key: JobKey, before: String?) {
        jobs[key]?.started = true
        jobs[key]?.before = before
    }

    mutating func setVault(_ key: JobKey, _ step: VaultSetupStep) {
        jobs[key]?.vault = step
    }

    mutating func end(_ key: JobKey, at date: Date = .now) {
        jobs[key]?.ended = date
    }

    /// Forgets a job, once it has ended and its outcome has been seen.
    mutating func clear(_ key: JobKey) {
        jobs[key] = nil
        keys.removeAll { $0 == key }
    }

    func phase(_ key: JobKey) -> Phase {
        guard let job = jobs[key] else { return .ended }
        return job.ended != nil ? .ended : job.started ? .running : .queued
    }

    /// The queued jobs that may start now, oldest first.
    var startable: [JobKey] { Self.startable(keys.map { ($0, phase($0)) }) }

    /// Which of `jobs`, oldest first, start now: a queued install while no other install runs
    /// on its machine and none is queued there before it; any other queued job at once.
    nonisolated static func startable(_ jobs: [(key: JobKey, phase: Phase)]) -> [JobKey] {
        var busy = Set(jobs.filter { $0.phase == .running && $0.key.isInstall }.map(\.key.hostId))
        var start: [JobKey] = []
        for (key, phase) in jobs where phase == .queued {
            if key.isInstall {
                guard !busy.contains(key.hostId) else { continue }
                busy.insert(key.hostId)
            }
            start.append(key)
        }
        return start
    }

    /// The account ids the logins still running on `hostId` hold: a new account cannot take
    /// one, as the daemon also refuses.
    func pendingIds(on hostId: HostId) -> [AccountId] {
        keys.compactMap { key in
            guard key.hostId == hostId, case .add(let id) = key.kind, phase(key) != .ended else { return nil }
            return id
        }
    }
}

extension JobKey {
    var isInstall: Bool {
        if case .install = kind { return true }
        return false
    }
}

extension Fleet {
    /// Queues installing or updating `provider`'s CLI on `hostId`, unless that is queued or runs
    /// already. It starts once the machine's installs before it end.
    @discardableResult
    func install(_ provider: Provider, on hostId: HostId) -> JobKey {
        run(JobKey(hostId: hostId, kind: .install(provider)), TerminalConnection(hostId: hostId, terminalId: nil, install: provider))
    }

    /// Starts adding `account` on `hostId`: the provider's own login, in its terminal. The id is
    /// the account's: one id can be added only once at a time.
    @discardableResult
    func addAccount(_ account: NewAccount, on hostId: HostId) -> JobKey {
        run(JobKey(hostId: hostId, kind: .add(account.accountId)), TerminalConnection(hostId: hostId, terminalId: nil, account: account))
    }

    /// Starts logging `accountId` in again on `hostId`.
    @discardableResult
    func relogin(_ accountId: AccountId, on hostId: HostId) -> JobKey {
        run(JobKey(hostId: hostId, kind: .relogin(accountId)), TerminalConnection(hostId: hostId, terminalId: nil, relogin: accountId))
    }

    /// Starts signing `hostId` in to the AWS `profile`, with the AWS CLI's device-code sign-in.
    @discardableResult
    func signInToAws(_ profile: String, on hostId: HostId) -> JobKey {
        run(JobKey(hostId: hostId, kind: .awsSignIn(profile: profile)),
            TerminalConnection(hostId: hostId, terminalId: nil, awsProfile: profile))
    }

    /// Runs an ended job again, as it was started.
    func retry(_ key: JobKey) {
        guard let job = activity[key] else { return }
        switch key.kind {
        case .install(let provider): install(provider, on: key.hostId)
        case .add: if let account = job.connection?.account { addAccount(account, on: key.hostId) }
        case .relogin(let id): relogin(id, on: key.hostId)
        case .awsSignIn(let profile): signInToAws(profile, on: key.hostId)
        case .vault: setUpVault(on: key.hostId)
        }
    }

    /// Clears every job that has ended.
    func clearFinished() {
        for key in activity.keys where activity.phase(key) == .ended { activity.clear(key) }
    }

    private func run(_ key: JobKey, _ connection: TerminalConnection) -> JobKey {
        if activity.enqueue(key, connection) { startQueued() }
        return key
    }

    /// Opens the terminal of each queued job whose turn it is. A job runs whether its terminal
    /// is on screen or not; the emulator takes its own size once it is shown.
    private func startQueued() {
        for key in activity.startable {
            guard let connection = activity[key]?.connection else { continue }
            let before: String? = if case .install(let provider) = key.kind { cliVersion(provider, on: key.hostId) } else { nil }
            activity.start(key, before: before)
            connection.ended = { [weak self, weak connection] in
                // A job run again has a new connection; the old one's end is not its.
                guard let self, let connection, self.activity[key]?.connection === connection else { return }
                self.activity.end(key)
                self.startQueued()
            }
            connection.connect(client: client, sessionId: nil, cols: 100, rows: 30)
        }
    }

    /// The version of `provider`'s CLI `hostId` lists, as its number: empty when the machine
    /// cannot read it, `nil` when it is not installed.
    func cliVersion(_ provider: Provider, on hostId: HostId) -> String? {
        let status = machines.first { $0.hostId == hostId }?.providers.first { $0.provider == provider }
        guard let status, status.installed else { return nil }
        return status.version.map(ProviderVersions.number) ?? ""
    }

    /// Where the job `key` stands, from its terminal and what the machine lists now.
    func progress(_ key: JobKey) -> JobProgress {
        guard let job = activity[key] else { return .ended }
        guard job.started else { return .queued }
        if case .vault = key.kind { return JobProgress(vault: job.vault) }
        guard let connection = job.connection else { return .ended }
        let machine = machines.first { $0.hostId == key.hostId }
        switch key.kind {
        case .install(let provider):
            return JobProgress(install: connection.state, before: job.before, after: cliVersion(provider, on: key.hostId))
        case .add(let id):
            return JobProgress(connection.state, added: machine?.accounts.contains { $0.accountId == id } == true)
        case .relogin:
            return JobProgress(relogin: connection.state)
        case .awsSignIn:
            return JobProgress(AwsSignInStatus(state: connection.state, refusal: connection.refusal,
                                               terminalId: connection.terminalId, terminals: machine?.terminals ?? []))
        case .vault:
            return JobProgress(vault: job.vault)
        }
    }

    /// Cancels the login `key` runs: closes its terminal on the machine, which hangs up the
    /// provider's login, so it adds no account. Its terminal then shows it ended.
    func cancelLogin(_ key: JobKey) async throws {
        guard let terminalId = activity[key]?.connection?.terminalId else {
            throw HerderError.Local(detail: "the login has no terminal on the machine yet")
        }
        try await client.closeTerminal(hostId: key.hostId, terminalId: terminalId)
    }
}

/// Where a job stands, as its row and its detail say it.
enum JobProgress: Equatable {
    /// Waiting for an install before it on the same machine to end.
    case queued
    /// Opening the terminal on the machine.
    case starting
    /// The login runs, waiting on the user to finish the provider's own login.
    case running
    /// The AWS CLI printed its device code; the user approves it in a browser.
    case approve(AwsDeviceCode)
    /// The installer or updater runs; it needs no one.
    case installing
    /// The login ended and the machine lists the account.
    case added
    /// The login or AWS sign-in ended signed in.
    case signedIn
    /// The login's terminal ended without the account showing up, or was cancelled.
    case ended
    /// The install ended, and the machine lists the CLI at this version.
    case installed(String)
    /// The update ended, and the machine lists the CLI at a new version.
    case updated(from: String, to: String)
    /// The update ended, and the machine lists the CLI at the version it had.
    case upToDate(String)
    /// The machine installs its vault, or with `install` off links to the one it has.
    case settingUpVault(install: Bool)
    /// This device pairs with the new vault.
    case pairingVault
    /// The vault is paired here, named so, and the machine linked to it.
    case vaultReady(String)
    /// The machine's herder cannot set up a vault from here: install it there by hand.
    case needsNewerHerder
    case failed(String)

    /// A login that adds an account: `added` tells whether the machine lists it.
    init(_ state: TerminalConnection.State, added: Bool) {
        switch state {
        case .connecting: self = added ? .added : .starting
        case .attached: self = added ? .added : .running
        case .exited: self = added ? .added : .ended
        case .failed(let message): self = .failed(message)
        }
    }

    /// A login run again on an account the machine lists already: the provider's login exits
    /// with 0 once it signed in.
    init(relogin state: TerminalConnection.State) {
        switch state {
        case .connecting: self = .starting
        case .attached: self = .running
        case .exited(0): self = .signedIn
        case .exited(let code?): self = .failed("The login exited with \(code).")
        case .exited(nil): self = .ended
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

    /// An AWS sign-in's, from the status its machine reports.
    init(_ status: AwsSignInStatus) {
        switch status {
        case .starting: self = .starting
        case .waiting(let device): self = .approve(device)
        case .signedIn: self = .signedIn
        case .failed(let code): self = .failed(code.map { "aws sso login exited with \($0)." } ?? "aws sso login was stopped.")
        case .refused(let reason): self = .failed(reason)
        case .noSuchProfile: self = .failed("The machine has no such profile in ~/.aws/config.")
        case .notInstalled: self = .failed("The AWS CLI is not installed on the machine.")
        case .unlisted, .offline: self = .failed(status.label)
        }
    }

    /// The Activity list's groups, in the order it shows them.
    enum Group: CaseIterable {
        case needsYou, running, queued, done

        var title: String {
            switch self {
            case .needsYou: "Needs you"
            case .running: "Running"
            case .queued: "Queued"
            case .done: "Done"
            }
        }
    }

    var group: Group {
        switch self {
        case .running, .approve: .needsYou
        case .starting, .installing, .settingUpVault, .pairingVault: .running
        case .queued: .queued
        case .added, .signedIn, .ended, .installed, .updated, .upToDate, .vaultReady, .needsNewerHerder, .failed: .done
        }
    }

    var finished: Bool { group == .done }

    var failed: Bool {
        switch self {
        case .failed, .needsNewerHerder: true
        default: false
        }
    }
}

extension JobKey {
    /// The job as its row names it: what it does and where, or once it ended, what it did.
    /// `provider` is the CLI or account's, `updating` whether an install found the CLI there.
    func title(machine: String, provider: Provider, progress: JobProgress, updating: Bool) -> String {
        let name = ModelCatalog.providerName(provider)
        switch (kind, progress) {
        case (.install, .installed): return "Installed \(name) on \(machine)"
        case (.install, .updated): return "Updated \(name) on \(machine)"
        case (.install, .upToDate): return "\(name) on \(machine) is up to date"
        case (.install, .failed): return "\(name) on \(machine) failed"
        case (.install, _): return updating ? "Update \(name) on \(machine)" : "Install \(name) on \(machine)"
        case (.add(let id), .added): return "Added \(name) · \(id) on \(machine)"
        case (.add(let id), .failed): return "Sign-in to \(name) · \(id) on \(machine) failed"
        case (.add(let id), _): return "Sign in to \(name) · \(id) on \(machine)"
        case (.relogin(let id), .signedIn): return "Signed \(id) in again on \(machine)"
        case (.relogin(let id), .failed): return "Logging \(id) in again on \(machine) failed"
        case (.relogin(let id), _): return "Log \(id) in again on \(machine)"
        case (.awsSignIn(let profile), .signedIn): return "Signed in to AWS \(profile) on \(machine)"
        case (.awsSignIn(let profile), .failed): return "AWS sign-in to \(profile) on \(machine) failed"
        case (.awsSignIn(let profile), _): return "AWS sign-in to \(profile) on \(machine)"
        case (.vault, .vaultReady(let name)): return "Ran \(name)"
        case (.vault, .failed), (.vault, .needsNewerHerder): return "Running a vault on \(machine) failed"
        case (.vault, .settingUpVault(install: false)): return "Pair and link the vault on \(machine)"
        case (.vault, _): return "Run a vault on \(machine)"
        }
    }
}

extension JobProgress {
    /// The line under a job's title: its step, the versions it went between, or why it failed.
    /// `before` is an install's CLI version when it started.
    func detail(machine: String, before: String?) -> String {
        switch self {
        case .queued: "Queued · one installer at a time on \(machine)"
        case .starting: "Opening its terminal on \(machine)"
        case .installing:
            before.map { $0.isEmpty ? "Running the vendor’s updater" : "From \($0) · running the vendor’s updater" }
                ?? "Running the vendor’s installer"
        case .running: "Waiting for you to finish the provider’s login"
        case .approve(let device): "Approve code \(device.userCode) in the browser"
        case .installed(let version): version.isEmpty ? "Installed" : version
        case .updated(let from, let to): from.isEmpty || to.isEmpty ? "Updated" : "\(from) → \(to)"
        case .upToDate(let version): version.isEmpty ? "Already the latest" : "Already at \(version)"
        case .added: "Sessions can use it now"
        case .signedIn: "Signed in"
        case .ended: "Stopped before it signed in"
        case .settingUpVault(install: true): "Installing the vault’s service on \(machine) and linking to it"
        case .settingUpVault(install: false): "Linking \(machine) to the vault installed there"
        case .pairingVault: "Pairing this device with the vault"
        case .vaultReady: "Paired here · \(machine) fetches secrets from it"
        case .needsNewerHerder: "Its herder cannot do this from here: run \(VaultChoices.installCommand) on \(machine)"
        case .failed(let message): message
        }
    }

    /// The short outcome its badge shows.
    var label: String {
        switch self {
        case .queued: "Queued"
        case .starting: "Starting"
        case .running: "Waiting for you"
        case .approve: "Approve in browser"
        case .installing: "Installing"
        case .added: "Added"
        case .signedIn: "Signed in"
        case .ended: "Stopped"
        case .installed(let version): version.isEmpty ? "Installed" : "Installed \(version)"
        case .updated(let from, let to): from.isEmpty || to.isEmpty ? "Updated" : "\(from) → \(to)"
        case .upToDate(let version): version.isEmpty ? "Up to date" : "Up to date \(version)"
        case .settingUpVault(let install): install ? "Installing" : "Linking"
        case .pairingVault: "Pairing"
        case .vaultReady: "Ready"
        case .needsNewerHerder: "Needs a newer herder"
        case .failed: "Failed"
        }
    }
}
