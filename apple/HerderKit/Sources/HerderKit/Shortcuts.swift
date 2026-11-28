import Observation
import SwiftUI
#if os(macOS)
import AppKit
import KeyboardShortcuts
#endif

/// Every keyboard shortcut herder has, in one list: the menu bar's, the session's and the
/// artifact viewer's. On the Mac each is rebindable in Settings → Shortcuts; iPad shows the
/// list. ⌘/ shows them all over the window (`CheatSheet`).
enum Shortcut: String, CaseIterable, Identifiable, Sendable {
    case newSession, newChat, speakToChat, reconnect, cheatSheet
    case sidebar, sessionList, inspector
    case board, chats, bots, providers, skills, hub, back
    case dictate, send, find, previousMatch, nextMatch, terminal
    case previousArtifact, nextArtifact, fitWidth, zoomOut, actualSize, zoomIn, saveArtifact

    var id: Self { self }

    /// The groups Settings and the cheat sheet show them in, in this order.
    enum Group: String, CaseIterable, Identifiable, Sendable {
        case general = "General"
        case view = "View"
        case go = "Go"
        case session = "Session"
        case artifacts = "Artifacts"

        var id: Self { self }
        var shortcuts: [Shortcut] { Shortcut.allCases.filter { $0.group == self } }
    }

    var group: Group {
        switch self {
        case .newSession, .newChat, .speakToChat, .reconnect, .cheatSheet: .general
        case .sidebar, .sessionList, .inspector: .view
        case .board, .chats, .bots, .providers, .skills, .hub, .back: .go
        case .dictate, .send, .find, .previousMatch, .nextMatch, .terminal: .session
        case .previousArtifact, .nextArtifact, .fitWidth, .zoomOut, .actualSize, .zoomIn, .saveArtifact: .artifacts
        }
    }

    var title: String {
        switch self {
        case .newSession: "New Session"
        case .newChat: "New Chat"
        case .speakToChat: "Speak to Chat"
        case .reconnect: "Reconnect"
        case .cheatSheet: "Keyboard Shortcuts"
        case .sidebar: "Collapse or Expand the Sidebar"
        case .sessionList: "Hide or Show the Session List"
        case .inspector: "Events and Statistics"
        case .board, .chats, .bots, .providers, .skills, .hub: section?.title ?? ""
        case .back: "Back"
        case .dictate: "Dictate"
        case .send: "Send"
        case .find: "Find in Transcript"
        case .previousMatch: "Previous Match"
        case .nextMatch: "Next Match"
        case .terminal: "Terminal or Chat"
        case .previousArtifact: "Previous Artifact"
        case .nextArtifact: "Next Artifact"
        case .fitWidth: "Fit Width"
        case .zoomOut: "Zoom Out"
        case .actualSize: "Actual Size"
        case .zoomIn: "Zoom In"
        case .saveArtifact: "Save As…"
        }
    }

    /// What Settings says under it, where the title alone does not say enough.
    var note: String? {
        switch self {
        case .speakToChat:
            "Works with herder in the background too. Dictates into the session or draft on show; with none open, starts a chat. Press it again, or Return, to stop."
        default: nil
        }
    }

    /// The sidebar section it goes to, for the Go shortcuts.
    var section: AppCommand.Section? {
        switch self {
        case .board: .board
        case .chats: .chats
        case .bots: .bots
        case .providers: .providers
        case .skills: .skills
        case .hub: .hub
        default: nil
        }
    }

    /// The Go shortcut of `section`.
    static func go(_ section: AppCommand.Section) -> Shortcut {
        allCases.first { $0.section == section } ?? .board
    }

    /// Its key until rebound.
    var defaultKey: KeyboardShortcut {
        switch self {
        case .newSession: KeyboardShortcut("n")
        case .newChat: KeyboardShortcut("n", modifiers: [.command, .option])
        // ⌥⌘D hides the Dock and ⌃⌘D looks a word up.
        case .speakToChat: KeyboardShortcut("d", modifiers: [.control, .option, .command])
        case .reconnect: KeyboardShortcut("r")
        case .cheatSheet: KeyboardShortcut("/")
        case .sidebar: KeyboardShortcut("\\", modifiers: [.command, .shift])
        case .sessionList: KeyboardShortcut("\\")
        case .inspector: KeyboardShortcut("i", modifiers: [.command, .option])
        case .board, .chats, .bots, .providers, .skills, .hub:
            KeyboardShortcut(KeyEquivalent(section?.key ?? "1"))
        case .back, .previousArtifact: KeyboardShortcut("[")
        case .dictate: KeyboardShortcut("d", modifiers: [.command, .shift])
        case .send: KeyboardShortcut(.return)
        case .find: KeyboardShortcut("f")
        case .previousMatch: KeyboardShortcut("g", modifiers: [.command, .shift])
        case .nextMatch: KeyboardShortcut("g")
        case .terminal: KeyboardShortcut("`")
        case .nextArtifact: KeyboardShortcut("]")
        case .fitWidth: KeyboardShortcut("9")
        case .zoomOut: KeyboardShortcut("-")
        case .actualSize: KeyboardShortcut("0")
        case .zoomIn: KeyboardShortcut("=")
        case .saveArtifact: KeyboardShortcut("s")
        }
    }

    /// The window's shortcuts and the artifact viewer's never show together, so they may share
    /// keys (⌘[ is Back in one and Previous Artifact in the other); within each, keys are one
    /// shortcut's.
    var scope: Int { group == .artifacts ? 1 : 0 }

    /// "⇧⌘D": the key as the menu bar writes it, modifiers in the Mac's order.
    static func label(_ key: KeyboardShortcut) -> String {
        let modifiers: [(EventModifiers, String)] = [(.control, "⌃"), (.option, "⌥"), (.shift, "⇧"), (.command, "⌘")]
        let held = modifiers.filter { key.modifiers.contains($0.0) }.map(\.1).joined()
        let character: String = switch key.key {
        case .return: "↩"
        case .escape: "⎋"
        case .tab: "⇥"
        case .space: "Space"
        case .delete: "⌫"
        case .upArrow: "↑"
        case .downArrow: "↓"
        case .leftArrow: "←"
        case .rightArrow: "→"
        default: String(key.key.character).uppercased()
        }
        return held + character
    }
}

/// A titled group of keys as the cheat sheet shows it: each row a name and its key (`nil`
/// when it has none).
struct KeyGroup: Identifiable, Equatable {
    let title: String
    let rows: [Row]
    var id: String { title }

    struct Row: Identifiable, Equatable {
        let title: String
        let key: String?
        var id: String { title }
    }

    /// Keys that are not commands and stay as they are: typing in the composer and moving
    /// down a list. Settings and the cheat sheet list them with the rest.
    static let fixed: [KeyGroup] = {
        var groups = [KeyGroup(title: "Composer", rows: [
            Row(title: "Send", key: "↩"), Row(title: "New Line", key: "⇧↩"), Row(title: "Complete a Skill", key: "⇥"),
        ])]
        #if os(macOS)
        groups.append(KeyGroup(title: "Board", rows: [
            Row(title: "Next Task", key: "J"), Row(title: "Previous Task", key: "K"), Row(title: "Open", key: "↩"),
            Row(title: "Archive", key: "E"), Row(title: "Undo", key: "⌘Z"),
        ]))
        #endif
        return groups
    }()
}

/// The keys every `Shortcut` has now: the person's own on the Mac, the defaults on iPad. Views
/// read it, so a key rebound in Settings shows at once in the menu bar, the tooltips and the
/// cheat sheet.
@MainActor
@Observable
final class Keymap {
    static let shared = Keymap()

    /// The keys as bound; one cleared in Settings has none.
    private(set) var keys: [Shortcut: KeyboardShortcut]
    /// While Settings records a key, the app's keys step aside, so the one pressed is recorded
    /// rather than done.
    private(set) var recording = false
    /// Changes with every key bound, so a menu item, which SwiftUI keeps, is made again.
    private(set) var revision = 0
    @ObservationIgnored private var observers: [NSObjectProtocol] = []

    init() {
        keys = Self.bound()
        #if os(macOS)
        // KeyboardShortcuts says so when Settings changes a key, and when its recorder starts or
        // stops; the names are its own.
        let center = NotificationCenter.default
        observers = [
            center.addObserver(forName: Notification.Name("KeyboardShortcuts_shortcutByNameDidChange"),
                               object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.reload() }
            },
            center.addObserver(forName: Notification.Name("KeyboardShortcuts_recorderActiveStatusDidChange"),
                               object: nil, queue: .main) { [weak self] note in
                let active = note.userInfo?["isActive"] as? Bool ?? false
                MainActor.assumeIsolated { self?.record(active) }
            },
        ]
        #endif
    }

    /// The key `shortcut` answers to now; none while Settings records one.
    func active(_ shortcut: Shortcut) -> KeyboardShortcut? {
        recording ? nil : keys[shortcut]
    }

    /// "⇧⌘D", or `nil` when it has no key.
    func label(_ shortcut: Shortcut) -> String? {
        keys[shortcut].map(Shortcut.label)
    }

    /// A tooltip: "Dictate, on this device (⇧⌘D)", or the text alone when it has no key.
    func help(_ text: String, _ shortcut: Shortcut) -> String {
        label(shortcut).map { "\(text) (\($0))" } ?? text
    }

    /// The cheat sheet's two columns: the window's shortcuts and the composer's keys, then the
    /// session's, the Board's and the artifact viewer's; each with its key as bound now.
    var columns: [[KeyGroup]] {
        func group(_ group: Shortcut.Group) -> KeyGroup {
            KeyGroup(title: group.rawValue, rows: group.shortcuts.map { KeyGroup.Row(title: $0.title, key: label($0)) })
        }
        let fixed = KeyGroup.fixed
        return [
            [group(.general), group(.view), group(.go)] + fixed.prefix(1),
            [group(.session)] + fixed.dropFirst() + [group(.artifacts)],
        ]
    }

    /// The other shortcut that already has `key`, where both could be pressed.
    func conflict(_ key: KeyboardShortcut, for shortcut: Shortcut) -> Shortcut? {
        Shortcut.allCases.first { other in
            other != shortcut && other.scope == shortcut.scope && keys[other].map { Self.same($0, key) } == true
        }
    }

    func reload() {
        keys = Self.bound()
        revision += 1
    }

    private func record(_ active: Bool) {
        recording = active
        revision += 1
    }

    static func same(_ one: KeyboardShortcut, _ other: KeyboardShortcut) -> Bool {
        one.key == other.key && one.modifiers == other.modifiers
    }

    private static func bound() -> [Shortcut: KeyboardShortcut] {
        var keys: [Shortcut: KeyboardShortcut] = [:]
        for shortcut in Shortcut.allCases {
            #if os(macOS)
            keys[shortcut] = shortcut.name.shortcut?.toSwiftUI
            #else
            keys[shortcut] = shortcut.defaultKey
            #endif
        }
        return keys
    }
}

#if os(macOS)
extension Shortcut {
    /// Its name with KeyboardShortcuts, which keeps the person's key in the user defaults.
    var name: KeyboardShortcuts.Name { Self.names[self] ?? KeyboardShortcuts.Name(rawValue) }

    /// Made once: a name made registers its default.
    private static let names: [Shortcut: KeyboardShortcuts.Name] = Dictionary(
        uniqueKeysWithValues: allCases.map { ($0, KeyboardShortcuts.Name($0.rawValue, initial: carbon($0.defaultKey))) })

    /// `key` as KeyboardShortcuts keeps it: by the key's code on a US keyboard.
    static func carbon(_ key: KeyboardShortcut) -> KeyboardShortcuts.Shortcut? {
        let code: KeyboardShortcuts.Key? = switch key.key {
        case .return: .return
        case .escape: .escape
        case .tab: .tab
        case .space: .space
        default: keyCodes[key.key.character]
        }
        guard let code else { return nil }
        var modifiers: NSEvent.ModifierFlags = []
        if key.modifiers.contains(.command) { modifiers.insert(.command) }
        if key.modifiers.contains(.option) { modifiers.insert(.option) }
        if key.modifiers.contains(.control) { modifiers.insert(.control) }
        if key.modifiers.contains(.shift) { modifiers.insert(.shift) }
        return KeyboardShortcuts.Shortcut(code, modifiers: modifiers)
    }

    private static let keyCodes: [Character: KeyboardShortcuts.Key] = [
        "a": .a, "b": .b, "c": .c, "d": .d, "e": .e, "f": .f, "g": .g, "h": .h, "i": .i, "j": .j,
        "k": .k, "l": .l, "m": .m, "n": .n, "o": .o, "p": .p, "q": .q, "r": .r, "s": .s, "t": .t,
        "u": .u, "v": .v, "w": .w, "x": .x, "y": .y, "z": .z,
        "0": .zero, "1": .one, "2": .two, "3": .three, "4": .four, "5": .five, "6": .six, "7": .seven,
        "8": .eight, "9": .nine,
        "/": .slash, "\\": .backslash, "`": .backtick, "[": .leftBracket, "]": .rightBracket,
        "-": .minus, "=": .equal, ",": .comma, ".": .period, ";": .semicolon, "'": .quote,
    ]
}
#endif

extension View {
    /// The key `shortcut` has now, kept up with Settings.
    func keyboardShortcut(_ shortcut: Shortcut) -> some View {
        modifier(ShortcutKey(shortcut: shortcut))
    }

    /// A tooltip that ends with `shortcut`'s key as bound now: "Dictate, on this device (⇧⌘D)".
    func help(_ text: String, shortcut: Shortcut) -> some View {
        modifier(ShortcutHelp(text: text, shortcut: shortcut))
    }
}

private struct ShortcutKey: ViewModifier {
    let shortcut: Shortcut

    func body(content: Content) -> some View {
        let keymap = Keymap.shared
        content
            .keyboardShortcut(keymap.active(shortcut))
            // A menu item keeps its first key unless it is made again.
            .id(keymap.revision)
    }
}

private struct ShortcutHelp: ViewModifier {
    let text: String
    let shortcut: Shortcut

    func body(content: Content) -> some View {
        content.tooltip(Keymap.shared.help(text, shortcut))
    }
}

extension View {
    /// A tooltip on hover. On the Mac it is AppKit's own (`NSView.toolTip`) on a view behind
    /// this one: SwiftUI's `.help` showed none over the composer's and the header's buttons,
    /// though hover reached them. VoiceOver reads it as the hint.
    func tooltip(_ text: String) -> some View {
        #if os(macOS)
        background(ToolTipHost(text: text)).accessibilityHint(text)
        #else
        help(text)
        #endif
    }
}

#if os(macOS)
/// Holds a tooltip for the SwiftUI view in front of it, which takes the clicks.
private struct ToolTipHost: NSViewRepresentable {
    let text: String

    func makeNSView(context: Context) -> ToolTipView { ToolTipView() }

    func updateNSView(_ view: ToolTipView, context: Context) {
        if view.toolTip != text { view.toolTip = text }
    }
}

/// A view that is only its tooltip: AppKit shows it from the view's bounds as the pointer
/// rests there, and never takes a click.
final class ToolTipView: NSView {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}
#endif
