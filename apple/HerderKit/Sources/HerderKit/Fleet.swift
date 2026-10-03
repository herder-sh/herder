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

    public init(client: Client) {
        self.client = client
        machines = client.machines()
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
