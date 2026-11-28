import SwiftUI
#if os(macOS)
import KeyboardShortcuts
#endif

/// Every shortcut over the window, by group, with its key as bound now (⌘/, or Help → Keyboard
/// Shortcuts). Read-only: the Mac rebinds them in Settings → Shortcuts. Esc, a click outside or
/// ⌘/ again closes it.
struct CheatSheet: View {
    let close: () -> Void

    var body: some View {
        let keymap = Keymap.shared
        VStack(alignment: .leading, spacing: 18) {
            HStack {
                Text("Keyboard Shortcuts").font(Theme.title).foregroundStyle(Theme.text)
                Spacer()
                #if os(macOS)
                Text("Change them in Settings (⌘,)").font(Theme.meta).foregroundStyle(Theme.tertiary)
                #endif
            }
            HStack(alignment: .top, spacing: 36) {
                ForEach(Array(keymap.columns.enumerated()), id: \.offset) { _, groups in column(groups) }
            }
        }
        .padding(24)
        .frame(maxWidth: 720)
        .background(Theme.sheet, in: .rect(cornerRadius: 16))
        .overlay(RoundedRectangle(cornerRadius: 16).strokeBorder(Theme.stroke))
        .shadow(color: .black.opacity(0.4), radius: 24, y: 8)
        .padding(24)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("cheat-sheet")
        .background {
            // Esc closes it, as a sheet.
            Button("Close", action: close).keyboardShortcut(.cancelAction).hidden()
        }
    }

    private func column(_ groups: [KeyGroup]) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            ForEach(groups) { group in
                VStack(alignment: .leading, spacing: 6) {
                    Text(group.title.uppercased()).font(.caption2.weight(.semibold)).foregroundStyle(Theme.tertiary)
                    ForEach(group.rows) { row in
                        HStack(spacing: 12) {
                            Text(row.title).font(Theme.body).foregroundStyle(Theme.secondary).lineLimit(1)
                            Spacer(minLength: 8)
                            KeyCaps(label: row.key)
                        }
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A shortcut's key in a quiet cap, or a dash when it has none.
struct KeyCaps: View {
    let label: String?

    var body: some View {
        Text(label ?? "—")
            .font(.system(.callout, design: .rounded).weight(.medium))
            .monospacedDigit()
            .foregroundStyle(label == nil ? Theme.tertiary : Theme.text)
            .padding(.horizontal, 7)
            .frame(minWidth: 28, minHeight: 22)
            .background(Theme.raised.opacity(label == nil ? 0 : 0.7), in: .rect(cornerRadius: 5))
    }
}

extension View {
    /// The cheat sheet over the window while `AppCommands` says it shows.
    func cheatSheet() -> some View {
        modifier(CheatSheetOverlay())
    }
}

private struct CheatSheetOverlay: ViewModifier {
    func body(content: Content) -> some View {
        let commands = AppCommands.shared
        content.overlay {
            if commands.showsCheatSheet {
                ZStack {
                    Color.black.opacity(0.35).ignoresSafeArea()
                        .onTapGesture { commands.showsCheatSheet = false }
                    CheatSheet { commands.showsCheatSheet = false }
                }
                .transition(.opacity)
            }
        }
        .animation(.smooth(duration: 0.15), value: commands.showsCheatSheet)
    }
}

#if os(iOS)
/// The You page's row on iPad, whose keyboard has the same keys: opens the cheat sheet.
struct ShortcutsRow: View {
    var body: some View {
        if UIDevice.current.userInterfaceIdiom == .pad {
            VStack(alignment: .leading, spacing: 8) {
                SectionHeading(title: "Keyboard")
                SettingsGroup {
                    Button { AppCommands.shared.showsCheatSheet = true } label: {
                        HStack {
                            Text("Keyboard Shortcuts").font(.subheadline).foregroundStyle(Theme.text)
                            Spacer(minLength: 8)
                            KeyCaps(label: Keymap.shared.label(.cheatSheet))
                        }
                        .padding(.horizontal, Theme.cellPadding)
                        .frame(minHeight: Theme.rowHeight)
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("keyboard-shortcuts")
                }
            }
        }
    }
}
#endif

#if os(macOS)
/// herder's settings window (⌘,): its Shortcuts pane.
public struct HerderSettings: View {
    public init() {}

    public var body: some View {
        TabView {
            Tab("Shortcuts", systemImage: "keyboard") { ShortcutsSettings() }
        }
        .frame(width: 520, height: 600)
    }
}

/// Every shortcut by group, each with a recorder and a reset to its default. A key another
/// shortcut has is refused, saying which; KeyboardShortcuts refuses the system's and other
/// menu items' keys.
struct ShortcutsSettings: View {
    var body: some View {
        let keymap = Keymap.shared
        Form {
            ForEach(Shortcut.Group.allCases) { group in
                Section(group.rawValue) {
                    ForEach(group.shortcuts) { shortcut in row(shortcut, keymap: keymap) }
                }
            }
            ForEach(KeyGroup.fixed) { group in
                Section {
                    ForEach(group.rows) { row in
                        LabeledContent(row.title) { KeyCaps(label: row.key) }
                    }
                } header: {
                    Text(group.title)
                } footer: {
                    if group == KeyGroup.fixed.last {
                        Text("Typing and list keys stay as they are.").font(.footnote).foregroundStyle(.secondary)
                    }
                }
            }
            Section {
                HStack {
                    Spacer()
                    Button("Restore Defaults") {
                        KeyboardShortcuts.reset(Shortcut.allCases.map(\.name))
                    }
                    .disabled(Shortcut.allCases.allSatisfy { isDefault($0, keymap: keymap) })
                }
            }
        }
        .formStyle(.grouped)
        .accessibilityIdentifier("shortcuts-settings")
    }

    private func row(_ shortcut: Shortcut, keymap: Keymap) -> some View {
        LabeledContent {
            HStack(spacing: 6) {
                KeyboardShortcuts.Recorder(for: shortcut.name)
                    .shortcutValidation { key in
                        guard let key = key.toSwiftUI, let other = keymap.conflict(key, for: shortcut) else {
                            return .allow
                        }
                        return .disallow(reason: "\(Shortcut.label(key)) is \(other.title)’s. Change that one first.")
                    }
                Button {
                    KeyboardShortcuts.reset(shortcut.name)
                } label: {
                    Image(systemName: "arrow.counterclockwise")
                }
                .buttonStyle(.borderless)
                .disabled(isDefault(shortcut, keymap: keymap))
                .help("Back to \(Shortcut.label(shortcut.defaultKey))")
                .accessibilityLabel("Reset \(shortcut.title)")
            }
        } label: {
            Text(shortcut.title)
            if let note = shortcut.note { Text(note) }
        }
    }

    private func isDefault(_ shortcut: Shortcut, keymap: Keymap) -> Bool {
        keymap.keys[shortcut].map { Keymap.same($0, shortcut.defaultKey) } == true
    }
}
#endif
