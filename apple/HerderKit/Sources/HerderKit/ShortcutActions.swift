import Foundation
import Herder

/// What Siri and the Shortcuts app do in herder, for the intents in apple/App: on the fleet the
/// app's window shows, through the same commands as its menu bar. Nothing here approves a
/// tool call, answers a question or opens a terminal; those stay in the app, in front of you.
@MainActor
public enum ShortcutActions {
    /// A project, as Shortcuts lists it.
    public struct Project: Hashable, Sendable {
        /// Its `ProjectGroup.id`, the project's id.
        public let id: String
        public let name: String
        /// "1 waiting for you · 3 tasks".
        public let status: String
    }

    /// A session that is not archived, as Shortcuts lists it.
    public struct Session: Hashable, Sendable {
        /// Its `herder://session` link.
        public let id: String
        public let title: String
        /// Its project, or "Chat".
        public let place: String
        public let needsYou: Bool
    }

    /// Why an action could not be done, as Siri says it.
    public struct Problem: LocalizedError, CustomLocalizedStringResourceConvertible {
        public let message: String

        init(_ message: String) { self.message = message }

        public var errorDescription: String? { message }
        public var localizedStringResource: LocalizedStringResource { LocalizedStringResource(stringLiteral: message) }
    }

    /// How long an action waits for a machine to connect, as an app opened cold connects.
    static let patience: Duration = .seconds(15)

    // MARK: The app's fleet

    public static func projects(matching query: String = "") throws -> [Project] {
        projects(in: try fleet(), matching: query)
    }

    public static func sessions(matching query: String = "") throws -> [Session] {
        sessions(in: try fleet(), matching: query)
    }

    /// Opens a new chat with dictation listening.
    public static func speakToChat() {
        AppCommands.shared.send(.speakToChat)
    }

    /// Starts a chat with `prompt` on the machine New Chat picks, and opens it; returns the
    /// machine's name.
    public static func ask(_ prompt: String) async throws -> String {
        let fleet = try fleet()
        AppCommands.shared.send(.go(.chats))
        guard let draft = await waitFor({ Draft.chat(fleet: fleet) }) else {
            throw Problem("No machine is connected to chat on.")
        }
        try await start(draft, prompt: prompt, in: fleet)
        return machineName(draft.hostId, in: fleet)
    }

    /// Starts a session in the project with `prompt`, on the machine its New Session picks,
    /// and opens it; returns the machine's name.
    public static func newSession(inProject id: String, prompt: String) async throws -> String {
        let fleet = try fleet()
        AppCommands.shared.send(.openProject(id))
        guard let draft = await waitFor({ Draft.inProject(id, fleet: fleet) }) else {
            throw Problem("No connected machine has that project.")
        }
        try await start(draft, prompt: prompt, in: fleet)
        return machineName(draft.hostId, in: fleet)
    }

    public static func openProject(_ id: String) {
        AppCommands.shared.send(.openProject(id))
    }

    public static func openSession(_ id: String) throws {
        guard case .session(let key)? = URL(string: id).flatMap(AppLink.init(url:)) else {
            throw Problem("herder cannot find that session.")
        }
        AppCommands.shared.send(.openSession(key))
    }

    /// The sessions waiting for you; opens the first, or the Board when none is.
    public static func whatNeedsMe() throws -> [Session] {
        let waiting = try sessions().filter(\.needsYou)
        if let first = waiting.first {
            try openSession(first.id)
        } else {
            AppCommands.shared.send(.go(.board))
        }
        return waiting
    }

    // MARK: Over a fleet

    static func projects(in fleet: Fleet, matching query: String = "") -> [Project] {
        fleet.lists.projects.compactMap { group in
            guard let id = group.projectId, matches(group.name, query) else { return nil }
            return Project(id: id, name: group.name,
                           status: ProjectSessions.status(live: group.live, archived: group.archived))
        }
    }

    /// The sessions not archived, those waiting for you first, then the projects' in their
    /// order, then the chats.
    static func sessions(in fleet: Fleet, matching query: String = "") -> [Session] {
        let lists = fleet.lists
        let all = lists.projects.flatMap { group in group.live.map { (summary: $0, place: group.name) } }
            + lists.chats.map { (summary: $0, place: "Chat") }
        var seen: Set<SessionKey> = []
        let sessions = all.compactMap { entry -> Session? in
            let summary = entry.summary
            guard summary.state != .archived, matches(summary.title, query), seen.insert(summary.key).inserted
            else { return nil }
            return Session(id: link(summary.key), title: summary.title, place: entry.place,
                           needsYou: summary.state == .needsYou)
        }
        return sessions.filter(\.needsYou) + sessions.filter { !$0.needsYou }
    }

    /// `herder://session?host=…&session=…`, which `AppLink` reads back.
    static func link(_ key: SessionKey) -> String {
        var components = URLComponents()
        components.scheme = "herder"
        components.host = "session"
        // `queryItems` would leave a `&` or `=` in an id as it is, splitting the query there.
        let allowed = CharacterSet.urlQueryAllowed.subtracting(CharacterSet(charactersIn: "&=+"))
        components.percentEncodedQueryItems = [("host", key.hostId), ("session", key.sessionId)].map { name, value in
            URLQueryItem(name: name, value: value.addingPercentEncoding(withAllowedCharacters: allowed))
        }
        return components.string ?? ""
    }

    /// Starts `draft`'s session as its composer would with nothing changed: the machine's
    /// default provider, model and account, and the project's or the last permission mode.
    static func start(_ draft: Draft, prompt: String, in fleet: Fleet) async throws {
        let text = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { throw Problem("Say what to ask.") }
        let choice = fleet.draftChoice(on: draft.hostId, projectId: draft.projectId)
        guard let account = fleet.defaultAccount(on: draft.hostId, projectId: draft.projectId, provider: choice.provider)
        else {
            throw Problem("\(machineName(draft.hostId, in: fleet)) has no \(ModelCatalog.providerName(choice.provider)) account.")
        }
        let mode = fleet.machines.first { $0.hostId == draft.hostId }?.projects
            .first { $0.projectId == draft.projectId }?.defaultPermissionMode ?? ModePreference.mode(for: draft)
        switch await fleet.start(draft, accountId: account.accountId, model: choice.model, mode: mode, prompt: text).value {
        case .success(let key):
            AppCommands.shared.send(.openSession(key))
        case .failure(let error):
            throw Problem(describe(error))
        }
    }

    // MARK: Helpers

    private static func fleet() throws -> Fleet {
        switch HerderScene.opened {
        case .opened(let fleet): fleet
        case .failed(let message): throw Problem(message)
        }
    }

    private static func matches(_ text: String, _ query: String) -> Bool {
        let needle = query.trimmingCharacters(in: .whitespaces)
        return needle.isEmpty || text.localizedCaseInsensitiveContains(needle)
    }

    private static func machineName(_ hostId: HostId, in fleet: Fleet) -> String {
        fleet.machines.first { $0.hostId == hostId }?.name ?? hostId
    }

    /// What `make` gives once it gives something, for up to `patience`.
    private static func waitFor<T>(_ make: () -> T?) async -> T? {
        let deadline = ContinuousClock.now + patience
        while ContinuousClock.now < deadline {
            if let made = make() { return made }
            try? await Task.sleep(for: .milliseconds(250))
        }
        return make()
    }
}
