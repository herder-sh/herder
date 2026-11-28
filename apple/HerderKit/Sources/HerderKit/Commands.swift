import Observation
import SwiftUI
#if os(macOS)
import AppKit
import KeyboardShortcuts
#endif

/// What the menu bar, the system-wide hotkey and Siri (`ShortcutActions`) ask the fleet's
/// window to do.
enum AppCommand: Hashable, Sendable {
    /// ⌘N: a session where the window is, as the sidebar's New Session.
    case newSession
    case newChat
    /// Dictation in the session or draft on show; with none open, a new chat listening, so you
    /// just talk.
    case speakToChat
    /// Reconnects every machine.
    case reconnect
    case go(Section)
    /// A project's pane, by its `ProjectGroup.id`.
    case openProject(String)
    /// A session, as its `herder://session` link opens it.
    case openSession(SessionKey)

    /// The sections ⌘1… go to, in the sidebar's order.
    enum Section: CaseIterable, Hashable, Sendable {
        case board, chats, bots, providers, skills, hub

        var title: String {
            switch self {
            case .board: "Board"
            case .chats: "Chats"
            case .bots: "Bots"
            case .providers: "Providers"
            case .skills: "Skills"
            case .hub: "Hub"
            }
        }

        var item: SidebarItem {
            switch self {
            case .board: .board
            case .chats: .chats
            case .bots: .bots(nil)
            case .providers: .providers
            case .skills: .skills
            case .hub: .hub
            }
        }

        /// ⌘1 for the Board, then on down the sidebar, until rebound (`Shortcut.go`).
        var key: Character {
            Character(String((Self.allCases.firstIndex(of: self) ?? 0) + 1))
        }
    }
}

/// Carries commands from the menu bar and the hotkey, which have no view of their own, to the
/// fleet's window, and dictation from Speak to Chat to the chat it opens. One for the app.
@MainActor
@Observable
final class AppCommands {
    static let shared = AppCommands()

    /// The command the fleet's window has yet to carry out.
    private(set) var pending: AppCommand?
    /// The draft, by `Draft.key`, whose composer starts dictating as it shows.
    private(set) var listening: String?
    /// Whether the keyboard shortcuts show over the window (⌘/).
    var showsCheatSheet = false
    /// Stops the dictation a composer is running, while one is.
    @ObservationIgnored private var dictating: (id: UUID, stop: () -> Void)?
    /// The composers on show, the latest last, each with its dictation's toggle.
    @ObservationIgnored private var shown: [(id: UUID, toggle: () -> Void)] = []

    /// Asks for `command`. Speak to Chat while a composer dictates stops it instead, so the
    /// same key starts and stops talking.
    func send(_ command: AppCommand) {
        if command == .speakToChat, let dictating {
            dictating.stop()
            return
        }
        pending = command
    }

    /// Whether a session's or a draft's composer is on show, which Speak to Chat dictates into.
    var composing: Bool { !shown.isEmpty }

    /// A composer came on show (`toggle` given) or went (`nil`).
    func composer(_ id: UUID, toggle: (() -> Void)?) {
        shown.removeAll { $0.id == id }
        if let toggle { shown.append((id, toggle)) }
    }

    /// Starts or stops dictating in the composer on show.
    func dictateInComposer() {
        shown.last?.toggle()
    }

    /// The pending command, taken by the window that carries it out.
    func take() -> AppCommand? {
        defer { pending = nil }
        return pending
    }

    /// Has the composer of the draft kept by `key` start dictating as it shows.
    func listen(for key: String) {
        listening = key
    }

    /// Whether the composer of the draft kept by `key` should start dictating; it asks once.
    func takeListening(for key: String?) -> Bool {
        guard let key, listening == key else { return false }
        listening = nil
        return true
    }

    /// A composer's dictation started (`stop` given) or ended (`nil`).
    func dictation(_ id: UUID, stop: (() -> Void)?) {
        if let stop {
            dictating = (id, stop)
        } else if dictating?.id == id {
            dictating = nil
        }
    }

    #if os(macOS)
    @ObservationIgnored private var followsHotkey = false

    /// Answers Speak to Chat's system-wide hotkey from now on: herder comes forward and does it.
    func followHotkey() {
        guard !followsHotkey else { return }
        followsHotkey = true
        KeyboardShortcuts.onKeyDown(for: Shortcut.speakToChat.name) { [weak self] in
            NSApp.activate()
            NSApp.windows.first { $0.isMiniaturized }?.deminiaturize(nil)
            self?.send(.speakToChat)
        }
    }
    #endif
}

/// What a command does in the fleet's window, worked out apart from it.
enum CommandRoute: Equatable {
    /// Opens a draft; `listens` starts dictating in it.
    case draft(Draft, listens: Bool)
    /// Starts or stops dictating in the composer on show.
    case dictate
    case sheet(AppSheet)
    case section(SidebarItem)
    /// Opens a session once its machine lists it, as a link does.
    case session(SessionKey)
    case reconnect
    /// Not yet: no machine to chat on is connected, but one is connecting.
    case wait
    /// It cannot be done; the toast says why.
    case problem(String)

    /// `command`, with `item` the section shown and `composing` whether a session's or a
    /// draft's composer is on show.
    @MainActor
    init(_ command: AppCommand, fleet: Fleet, item: SidebarItem, composing: Bool = false) {
        switch command {
        case .newSession:
            // A chat in Chats; a draft in the project pane's project; elsewhere the picker.
            if item == .chats, let draft = Draft.chat(fleet: fleet) {
                self = .draft(draft, listens: false)
            } else if let projectId = fleet.lists.project(for: item),
                      let draft = Draft.inProject(projectId, fleet: fleet) {
                self = .draft(draft, listens: false)
            } else {
                self = .sheet(.newSession)
            }
        case .speakToChat where composing:
            self = .dictate
        case .newChat, .speakToChat:
            if let draft = Draft.chat(fleet: fleet) {
                self = .draft(draft, listens: command == .speakToChat)
            } else if fleet.machines.contains(where: { $0.connection == .connecting && $0.hosts.isEmpty }) {
                self = .wait
            } else {
                self = .problem("No machine is connected to chat on")
            }
        case .reconnect:
            self = .reconnect
        case .go(let section):
            self = .section(section.item)
        case .openProject(let id):
            self = .section(.project(id))
        case .openSession(let key):
            self = .session(key)
        }
    }
}

/// The session on show's commands, for the menu bar.
struct SessionCommands {
    /// Opens or closes the terminal; `nil` where you may not have one (it is owner-only).
    var terminal: (() -> Void)?
    let showsTerminal: Bool
    /// `nil` while the terminal shows.
    var find: (() -> Void)?
    let inspector: () -> Void
    /// Hides or shows the list beside the session, on the Mac.
    var list: (() -> Void)?
    let listHidden: Bool
}

/// The composer on show's dictation, for the menu bar.
struct DictationCommand {
    let listening: Bool
    let toggle: () -> Void
}

extension FocusedValues {
    @Entry var sessionCommands: SessionCommands?
    @Entry var dictationCommand: DictationCommand?
}

/// The menu bar: herder's actions with their keys, so they can be found. Each goes to the
/// action the window already has; its key is the one `Keymap` has now.
public struct HerderCommands: Commands {
    @FocusedValue(\.sessionCommands) private var session
    @FocusedValue(\.dictationCommand) private var dictation
    @AppStorage("sidebarCollapsed") private var sidebarCollapsed = true

    public init() {}

    public var body: some Commands {
        // One window: ⌘N starts a session instead of opening another.
        CommandGroup(replacing: .newItem) {
            Button("New Session") { send(.newSession) }
                .keyboardShortcut(.newSession)
            Button("New Chat") { send(.newChat) }
                .keyboardShortcut(.newChat)
            // On the Mac its key is also the system-wide hotkey (`AppCommands.followHotkey`).
            Button("Speak to Chat") { send(.speakToChat) }
                .keyboardShortcut(.speakToChat)
        }
        CommandGroup(after: .sidebar) {
            Button(sidebarCollapsed ? "Expand Sidebar" : "Collapse Sidebar") { sidebarCollapsed.toggle() }
                .keyboardShortcut(.sidebar)
            Button(session?.listHidden == true ? "Show Session List" : "Hide Session List") { session?.list?() }
                .keyboardShortcut(.sessionList)
                .disabled(session?.list == nil)
            Button("Events and Statistics") { session?.inspector() }
                .keyboardShortcut(.inspector)
                .disabled(session == nil)
            Divider()
            Button("Reconnect") { send(.reconnect) }
                .keyboardShortcut(.reconnect)
        }
        CommandMenu("Go") {
            ForEach(AppCommand.Section.allCases, id: \.self) { section in
                Button(section.title) { send(.go(section)) }
                    .keyboardShortcut(Shortcut.go(section))
            }
        }
        CommandMenu("Session") {
            Button(dictation?.listening == true ? "Stop Dictating" : "Dictate") { dictation?.toggle() }
                .keyboardShortcut(.dictate)
                .disabled(dictation == nil)
            Button("Find in Transcript") { session?.find?() }
                .keyboardShortcut(.find)
                .disabled(session?.find == nil)
            Button(session?.showsTerminal == true ? "Back to the Chat" : "Terminal") { session?.terminal?() }
                .keyboardShortcut(.terminal)
                .disabled(session?.terminal == nil)
        }
        // herder has no help book: Help lists the keys instead.
        CommandGroup(replacing: .help) {
            Button("Keyboard Shortcuts") { AppCommands.shared.showsCheatSheet.toggle() }
                .keyboardShortcut(.cheatSheet)
        }
    }

    private func send(_ command: AppCommand) {
        AppCommands.shared.send(command)
    }
}
