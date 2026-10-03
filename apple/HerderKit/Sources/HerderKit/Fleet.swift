import Foundation
import Herder
import Observation

/// The paired machines and the state of every session they list, kept current from the
/// client's change notifications and one subscription per session, as the TUI keeps them.
@MainActor
@Observable
public final class Fleet {
    public let client: Client
    public private(set) var machines: [Machine]
    private(set) var sessions: [SessionKey: SessionModel] = [:]
    /// The last command a session refused, until its next command succeeds.
    private(set) var refusals: [SessionKey: String] = [:]
    @ObservationIgnored private var subscriptions: [SessionKey: Task<Void, Never>] = [:]
    /// Each machine's connection changes since the app opened, oldest first.
    private(set) var connectionLog: [HostId: [ConnectionChange]] = [:]

    public init(client: Client) {
        self.client = client
        machines = client.machines()
    }

    /// Replaces the machines without a client change, for tests.
    func setMachinesForTesting(_ machines: [Machine]) {
        self.machines = machines
    }

    /// What the lists show now.
    var lists: Lists { Lists(machines: machines, sessions: sessions) }

    /// Follows the client's changes until it stops; runs for as long as the fleet is shown.
    public func follow() async {
        // Subscribe before reading, so a change between `init` and now is not missed.
        let changes = client.changes()
        update(client.machines())
        while await changes.next() {
            update(client.machines())
        }
    }

    private func update(_ machines: [Machine]) {
        self.machines = machines
        for machine in machines {
            var log = connectionLog[machine.hostId] ?? []
            guard log.last?.state != machine.connection else { continue }
            log.append(ConnectionChange(at: .now, state: machine.connection))
            connectionLog[machine.hostId] = Array(log.suffix(100))
        }
        let listed = Set(machines.flatMap { machine in
            machine.sessions.map { SessionKey(hostId: machine.hostId, sessionId: $0.sessionId) }
        })
        for key in listed where subscriptions[key] == nil {
            subscriptions[key] = Task { await self.stream(key) }
        }
        for (key, task) in subscriptions where !listed.contains(key) {
            task.cancel()
            subscriptions[key] = nil
            sessions[key] = nil
        }
    }

    /// Folds a session's updates, cached state first, until it is no longer listed.
    private func stream(_ key: SessionKey) async {
        guard let subscription = try? client.subscribeSession(hostId: key.hostId, sessionId: key.sessionId)
        else { return }
        sessions[key] = sessions[key] ?? SessionModel(key: key)
        // Releasing the subscription when this returns unsubscribes.
        while let update = await subscription.next(), !Task.isCancelled {
            sessions[key]?.apply(update)
        }
    }

    /// Sends a command about a session; a refusal is kept to show with it.
    func send(_ command: CommandBody, about key: SessionKey) async {
        do {
            _ = try await client.send(hostId: key.hostId, command: command)
            refusals[key] = nil
        } catch {
            refusals[key] = describe(error)
        }
    }

    func answer(_ request: PendingRequest, allow: Bool) async {
        let key = request.session.key
        await send(
            .answerApproval(sessionId: key.sessionId, approvalId: request.requestId, decision: allow ? .allow : .deny),
            about: key)
    }

    func answer(_ request: PendingRequest, with answer: Answer) async {
        let key = request.session.key
        await send(.answerQuestion(sessionId: key.sessionId, questionId: request.requestId, answer: answer), about: key)
    }

    /// The account a new session runs on, so nobody has to pick one: the project's default
    /// account when it fits the provider, else the provider's account with the most room left.
    func defaultAccount(on hostId: HostId, projectId: String?, provider: Provider?) -> Account? {
        guard let machine = machines.first(where: { $0.hostId == hostId }) else { return nil }
        let preferred = machine.projects.first { $0.projectId == projectId }?.defaultAccount
        if let account = machine.accounts.first(where: { $0.accountId == preferred }),
           provider == nil || account.provider == provider {
            return account
        }
        return machine.accounts
            .filter { provider == nil || $0.provider == provider }
            .min { busiest($0) < busiest($1) }
    }

    private func busiest(_ account: Account) -> Double {
        account.usage.map(\.usedPercent).max() ?? 0
    }

    /// Creates a session, prompts it when a prompt is given, and returns it.
    func createSession(
        on hostId: HostId, repo: String?, projectId: String?, accountId: AccountId, model: String,
        mode: PermissionMode, prompt: String
    ) async throws -> SessionKey {
        let result = try await client.send(
            hostId: hostId,
            command: .createSession(
                repo: repo, projectId: projectId, branch: nil, accountId: accountId, provider: nil,
                model: model.isEmpty ? nil : model, permissionMode: mode, maxChildren: nil, failoverPin: nil))
        guard case .sessionCreated(let sessionId) = result else {
            throw HerderError.Local(detail: "the machine did not create a session")
        }
        let key = SessionKey(hostId: hostId, sessionId: sessionId)
        if !prompt.isEmpty {
            await send(.sendPrompt(sessionId: sessionId, text: prompt, images: []), about: key)
        }
        return key
    }

    /// Sends what the user typed: the answer to the session's oldest question when one is
    /// pending, else a prompt, queued behind the turn when one runs, as the TUI does.
    func submit(_ text: String, to key: SessionKey) async {
        guard let session = sessions[key] else { return }
        if let question = session.questions.first {
            await send(.answerQuestion(sessionId: key.sessionId, questionId: question.id, answer: .text(text: text)),
                       about: key)
            return
        }
        let outgoing = Outgoing(text: text)
        sessions[key]?.outbox.append(outgoing)
        await send(.sendPrompt(sessionId: key.sessionId, text: text, images: []), about: key)
        if let index = sessions[key]?.outbox.firstIndex(where: { $0.id == outgoing.id }) {
            sessions[key]?.outbox[index].state = refusals[key].map(Outgoing.State.failed) ?? .delivered
        }
    }

    /// Runs a queued prompt now: interrupts the turn, so the daemon starts the queue.
    func sendNow(_ key: SessionKey) async {
        await interrupt(key)
    }

    /// Drops a prompt that failed to send.
    func discard(_ outgoing: Outgoing, from key: SessionKey) {
        sessions[key]?.outbox.removeAll { $0.id == outgoing.id }
    }

    func interrupt(_ key: SessionKey) async {
        await send(.interrupt(sessionId: key.sessionId), about: key)
    }

    func setMode(_ mode: PermissionMode, of key: SessionKey) async {
        await send(.setPermissionMode(sessionId: key.sessionId, mode: mode), about: key)
    }

    /// Moves a session to an account, a model, or both, as the TUI's switch dialog does: the
    /// same account only changes the model, another account of the same provider switches
    /// account (then model), and another provider's account switches provider.
    func switchSession(_ key: SessionKey, to account: Account, model: String) async {
        guard let session = sessions[key] else { return }
        let model = model.trimmingCharacters(in: .whitespaces)
        if account.accountId == session.accountId {
            guard !model.isEmpty, model != session.model else {
                refusals[key] = "Already on this account: enter a new model."
                return
            }
            await send(.setModel(sessionId: key.sessionId, model: model), about: key)
        } else if account.provider == session.provider {
            await send(.switchAccount(sessionId: key.sessionId, accountId: account.accountId), about: key)
            if !model.isEmpty, refusals[key] == nil {
                await send(.setModel(sessionId: key.sessionId, model: model), about: key)
            }
        } else {
            await send(.switchProvider(sessionId: key.sessionId, accountId: account.accountId,
                                       model: model.isEmpty ? nil : model), about: key)
        }
    }

    func archive(_ key: SessionKey) async {
        await send(.archiveSession(sessionId: key.sessionId, force: false), about: key)
    }

    /// Pairs with the daemon a `herder://pair` link names.
    @discardableResult
    public func pair(link: String) async throws -> Machine {
        let machine = try await client.pair(link: link)
        update(client.machines())
        return machine
    }

    func rename(_ hostId: HostId, to name: String) throws {
        try client.rename(hostId: hostId, name: name)
        update(client.machines())
    }

    func forget(_ hostId: HostId) throws {
        try client.forget(hostId: hostId)
        update(client.machines())
    }

    /// The app is in the foreground: reconnects now and replaces dead connections.
    public func wake() {
        client.wake()
    }

    /// The app went to the background: saves the offline cache and stops retrying.
    public func suspend() {
        client.suspend()
    }
}

/// The profile this device keeps its pairings and offline cache in.
public enum Profile {
    case opened(Fleet)
    case failed(String)

    /// Opens the profile in `directory`, creating it if needed.
    @MainActor
    public static func open(at directory: URL, client name: String) -> Profile {
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            return .opened(Fleet(client: try Client.open(configDir: directory.path, client: name)))
        } catch {
            return .failed(describe(error))
        }
    }

    /// Opens the app's profile in Application Support.
    @MainActor
    public static func openDefault() -> Profile {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        #if os(iOS)
        let platform = "ios"
        #else
        let platform = "macos"
        #endif
        return open(
            at: support.appendingPathComponent("herder", isDirectory: true),
            client: "herder-\(platform)/\(version ?? "dev")")
    }
}

/// A machine's connection changing state.
struct ConnectionChange: Hashable {
    let at: Date
    let state: ConnectionState
}
