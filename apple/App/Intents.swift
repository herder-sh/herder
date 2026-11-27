import AppIntents
import HerderKit

// herder in Siri, Spotlight, the Shortcuts app, and the iPhone's Action button: thin intents
// over `ShortcutActions`, which carries each out in the app's window. None approves a tool
// call, answers a question or opens a terminal.

/// A new chat with dictation listening: talk, then press again or Return.
struct SpeakToChatIntent: AppIntent {
    static var title: LocalizedStringResource { "Speak to Chat" }
    static var description: IntentDescription {
        IntentDescription("Opens a new chat in herder with dictation listening.")
    }
    static var openAppWhenRun: Bool { true }

    @MainActor
    func perform() async throws -> some IntentResult {
        ShortcutActions.speakToChat()
        return .result()
    }
}

/// A chat started with what you say or type, on the machine New Chat picks.
struct AskIntent: AppIntent {
    static var title: LocalizedStringResource { "Ask herder" }
    static var description: IntentDescription {
        IntentDescription("Starts a new chat in herder with your prompt, and opens it.")
    }
    static var openAppWhenRun: Bool { true }

    @Parameter(title: "Prompt", requestValueDialog: "What should herder do?")
    var prompt: String

    static var parameterSummary: some ParameterSummary {
        Summary("Ask herder \(\.$prompt)")
    }

    @MainActor
    func perform() async throws -> some IntentResult & ProvidesDialog {
        let machine = try await ShortcutActions.ask(prompt)
        return .result(dialog: "Asked on \(machine).")
    }
}

/// A session in a project, started with a prompt.
struct NewSessionIntent: AppIntent {
    static var title: LocalizedStringResource { "New Session in Project" }
    static var description: IntentDescription {
        IntentDescription("Starts a session in a herder project with your prompt, and opens it.")
    }
    static var openAppWhenRun: Bool { true }

    @Parameter(title: "Project")
    var project: ProjectEntity

    @Parameter(title: "Prompt", requestValueDialog: "What should we build?")
    var prompt: String

    static var parameterSummary: some ParameterSummary {
        Summary("Start a session in \(\.$project): \(\.$prompt)")
    }

    @MainActor
    func perform() async throws -> some IntentResult & ProvidesDialog {
        let machine = try await ShortcutActions.newSession(inProject: project.id, prompt: prompt)
        return .result(dialog: "Started in \(project.name) on \(machine).")
    }
}

struct OpenProjectIntent: OpenIntent {
    static var title: LocalizedStringResource { "Open Project" }
    static var description: IntentDescription { IntentDescription("Opens a project in herder.") }

    @Parameter(title: "Project")
    var target: ProjectEntity

    @MainActor
    func perform() async throws -> some IntentResult {
        ShortcutActions.openProject(target.id)
        return .result()
    }
}

struct OpenSessionIntent: OpenIntent {
    static var title: LocalizedStringResource { "Open Session" }
    static var description: IntentDescription { IntentDescription("Opens a session or chat in herder.") }

    @Parameter(title: "Session")
    var target: SessionEntity

    @MainActor
    func perform() async throws -> some IntentResult {
        try ShortcutActions.openSession(target.id)
        return .result()
    }
}

/// How many sessions wait for you, opening the first.
struct WhatNeedsMeIntent: AppIntent {
    static var title: LocalizedStringResource { "What Needs Me" }
    static var description: IntentDescription {
        IntentDescription("Says how many herder sessions wait for you, and opens the first.")
    }
    static var openAppWhenRun: Bool { true }

    @MainActor
    func perform() async throws -> some IntentResult & ProvidesDialog & ReturnsValue<Int> {
        let waiting = try ShortcutActions.whatNeedsMe()
        let dialog: IntentDialog = switch waiting.count {
        case 0: "Nothing needs you."
        case 1: "1 session needs you: \(waiting[0].title)."
        default: "\(waiting.count) sessions need you. The first is \(waiting[0].title)."
        }
        return .result(value: waiting.count, dialog: dialog)
    }
}

// MARK: Entities

struct ProjectEntity: AppEntity {
    static var typeDisplayRepresentation: TypeDisplayRepresentation { "Project" }
    static var defaultQuery: ProjectQuery { ProjectQuery() }

    let id: String
    let name: String
    let status: String

    init(_ project: ShortcutActions.Project) {
        id = project.id
        name = project.name
        status = project.status
    }

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(title: "\(name)", subtitle: "\(status)")
    }
}

struct ProjectQuery: EntityStringQuery {
    @MainActor
    func entities(for identifiers: [String]) async throws -> [ProjectEntity] {
        try ShortcutActions.projects().filter { identifiers.contains($0.id) }.map(ProjectEntity.init)
    }

    @MainActor
    func entities(matching string: String) async throws -> [ProjectEntity] {
        try ShortcutActions.projects(matching: string).map(ProjectEntity.init)
    }

    @MainActor
    func suggestedEntities() async throws -> [ProjectEntity] {
        try ShortcutActions.projects().map(ProjectEntity.init)
    }
}

struct SessionEntity: AppEntity {
    static var typeDisplayRepresentation: TypeDisplayRepresentation { "Session" }
    static var defaultQuery: SessionQuery { SessionQuery() }

    /// Its `herder://session` link.
    let id: String
    let title: String
    let place: String
    let needsYou: Bool

    init(_ session: ShortcutActions.Session) {
        id = session.id
        title = session.title
        place = session.place
        needsYou = session.needsYou
    }

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(title: "\(title)", subtitle: needsYou ? "\(place) · Needs you" : "\(place)")
    }
}

struct SessionQuery: EntityStringQuery {
    @MainActor
    func entities(for identifiers: [String]) async throws -> [SessionEntity] {
        try ShortcutActions.sessions().filter { identifiers.contains($0.id) }.map(SessionEntity.init)
    }

    @MainActor
    func entities(matching string: String) async throws -> [SessionEntity] {
        try ShortcutActions.sessions(matching: string).map(SessionEntity.init)
    }

    @MainActor
    func suggestedEntities() async throws -> [SessionEntity] {
        try ShortcutActions.sessions().map(SessionEntity.init)
    }
}

// MARK: Siri

/// The phrases Siri knows without any setup; each names the app.
struct HerderShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: SpeakToChatIntent(),
            phrases: [
                "Speak to \(.applicationName)",
                "Talk to \(.applicationName)",
                "Dictate to \(.applicationName)",
            ],
            shortTitle: "Speak to Chat",
            systemImageName: "mic")
        AppShortcut(
            intent: AskIntent(),
            phrases: [
                "Ask \(.applicationName)",
                "Ask \(.applicationName) something",
                "New \(.applicationName) chat",
            ],
            shortTitle: "Ask herder",
            systemImageName: "bubble.left.and.bubble.right")
        AppShortcut(
            intent: NewSessionIntent(),
            phrases: [
                "Start a \(.applicationName) session in \(\.$project)",
                "New \(.applicationName) session in \(\.$project)",
                "Start a \(.applicationName) session",
            ],
            shortTitle: "New Session",
            systemImageName: "plus.bubble")
        AppShortcut(
            intent: WhatNeedsMeIntent(),
            phrases: [
                "What needs me in \(.applicationName)",
                "What does \(.applicationName) need",
                "Check \(.applicationName)",
            ],
            shortTitle: "What Needs Me",
            systemImageName: "exclamationmark.bubble")
        AppShortcut(
            intent: OpenProjectIntent(),
            phrases: [
                "Open \(\.$target) in \(.applicationName)",
                "Open a project in \(.applicationName)",
            ],
            shortTitle: "Open Project",
            systemImageName: "shippingbox")
        AppShortcut(
            intent: OpenSessionIntent(),
            phrases: [
                "Open a session in \(.applicationName)",
            ],
            shortTitle: "Open Session",
            systemImageName: "text.bubble")
    }
}
