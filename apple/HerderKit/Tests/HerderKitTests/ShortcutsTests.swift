import Foundation
@testable import HerderKit
import SwiftUI
import Testing
#if os(macOS)
import AppKit
import KeyboardShortcuts
#endif

@MainActor
struct ShortcutsTests {
    /// Today's keys stay the defaults: nobody's fingers have to learn the app again.
    @Test func theDefaultsAreTheKeysTheAppHad() {
        let expected: [Shortcut: String] = [
            .newSession: "⌘N", .newChat: "⌥⌘N", .speakToChat: "⌃⌥⌘D", .reconnect: "⌘R", .cheatSheet: "⌘/",
            .sidebar: "⇧⌘\\", .sessionList: "⌘\\", .inspector: "⌥⌘I",
            .board: "⌘1", .chats: "⌘2", .bots: "⌘3", .providers: "⌘4", .skills: "⌘5", .hub: "⌘6", .back: "⌘[",
            .dictate: "⇧⌘D", .send: "⌘↩", .find: "⌘F", .previousMatch: "⇧⌘G", .nextMatch: "⌘G", .terminal: "⌘`",
            .previousArtifact: "⌘[", .nextArtifact: "⌘]", .fitWidth: "⌘9", .zoomOut: "⌘-", .actualSize: "⌘0",
            .zoomIn: "⌘=", .saveArtifact: "⌘S",
        ]
        #expect(Set(expected.keys) == Set(Shortcut.allCases))
        for shortcut in Shortcut.allCases {
            #expect(Shortcut.label(shortcut.defaultKey) == expected[shortcut], "\(shortcut)")
        }
    }

    /// No two shortcuts that can be pressed in the same place start on one key.
    @Test func noTwoDefaultsClash() {
        for shortcut in Shortcut.allCases {
            let clash = Shortcut.allCases.first {
                $0 != shortcut && $0.scope == shortcut.scope && Keymap.same($0.defaultKey, shortcut.defaultKey)
            }
            #expect(clash == nil, "\(shortcut) and \(String(describing: clash)) share \(Shortcut.label(shortcut.defaultKey))")
        }
    }

    /// The menu bar's sections go by their own shortcuts, ⌘1 on down.
    @Test func eachSectionHasItsGoShortcut() {
        for section in AppCommand.Section.allCases {
            let shortcut = Shortcut.go(section)
            #expect(shortcut.section == section)
            #expect(shortcut.title == section.title)
            #expect(shortcut.defaultKey.key == KeyEquivalent(section.key))
        }
    }

    /// A tooltip names the key as bound now, and says nothing of one it no longer has.
    @Test func aTooltipEndsWithTheKey() {
        let keymap = Keymap()
        #expect(keymap.help("Dictate, on this device", .dictate) == "Dictate, on this device (⇧⌘D)")
        #expect(keymap.help("Send", .send) == "Send (⌘↩)")
        #expect(keymap.help("Find in the transcript", .find) == "Find in the transcript (⌘F)")
    }

    /// The cheat sheet lists every shortcut once, with its key, and the fixed keys after.
    @Test func theCheatSheetListsEveryShortcut() {
        let keymap = Keymap()
        let groups = keymap.columns.flatMap { $0 }
        for shortcut in Shortcut.allCases {
            let rows = groups.filter { $0.title == shortcut.group.rawValue }.flatMap(\.rows).filter { $0.title == shortcut.title }
            #expect(rows == [KeyGroup.Row(title: shortcut.title, key: keymap.label(shortcut))], "\(shortcut)")
        }
        for group in KeyGroup.fixed { #expect(groups.contains(group)) }
        #expect(groups.map(\.title) == ["General", "View", "Go", "Composer", "Session"]
            + KeyGroup.fixed.dropFirst().map(\.title) + ["Artifacts"])
    }

    /// Settings refuses a key another shortcut already has where both could be pressed.
    @Test func aKeyInUseIsRefused() {
        let keymap = Keymap()
        #expect(keymap.conflict(KeyboardShortcut("n"), for: .dictate) == .newSession)
        #expect(keymap.conflict(KeyboardShortcut("d", modifiers: [.command, .shift]), for: .dictate) == nil)
        // The viewer's ⌘[ and the window's are pressed in different places.
        #expect(keymap.conflict(KeyboardShortcut("["), for: .previousArtifact) == nil)
        #expect(keymap.conflict(KeyboardShortcut("x"), for: .dictate) == nil)
    }

    #if os(macOS)
    /// Each default is a key KeyboardShortcuts can keep, and comes back from it the same.
    @Test func everyDefaultRoundTripsThroughKeyboardShortcuts() throws {
        for shortcut in Shortcut.allCases {
            let carbon = try #require(Shortcut.carbon(shortcut.defaultKey), "\(shortcut)")
            let back = try #require(carbon.toSwiftUI, "\(shortcut)")
            #expect(Keymap.same(back, shortcut.defaultKey), "\(shortcut): \(Shortcut.label(back))")
            #expect(shortcut.name.initialShortcut == carbon)
        }
        // Speak to Chat keeps its name, so a key set before this list existed stays.
        #expect(Shortcut.speakToChat.name.rawValue == "speakToChat")
    }

    /// The mic's tooltip is AppKit's, behind the button, naming the key: SwiftUI's `.help`
    /// showed none there.
    @Test func theMicsTooltipNamesItsKey() async throws {
        let button = DictationButton(listening: false) {}
        let host = NSHostingView(rootView: button)
        host.frame = CGRect(x: 0, y: 0, width: 60, height: 60)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        for _ in 0..<3 {
            host.layoutSubtreeIfNeeded()
            try await Task.sleep(for: .milliseconds(50))
        }
        func tips(_ view: NSView) -> [ToolTipView] {
            ((view as? ToolTipView).map { [$0] } ?? []) + view.subviews.flatMap(tips)
        }
        let tip = try #require(tips(host).first)
        #expect(tip.toolTip == "Dictate, on this device (⇧⌘D)")
        #expect(tip.bounds.width >= 30 && tip.bounds.height >= 30)
        // It never takes the click from the button in front of it.
        #expect(tip.hitTest(CGPoint(x: 15, y: 15)) == nil)
    }

    /// A key rebound in Settings shows in the tooltip and the cheat sheet; a cleared one says
    /// so; reset brings the default back.
    @Test func aReboundKeyShowsEverywhere() {
        let name = Shortcut.dictate.name
        defer { KeyboardShortcuts.reset(name) }
        KeyboardShortcuts.setShortcut(.init(.l, modifiers: [.command, .option]), for: name)
        let keymap = Keymap()
        #expect(keymap.help("Dictate, on this device", .dictate) == "Dictate, on this device (⌥⌘L)")
        #expect(keymap.active(.dictate).map { Keymap.same($0, KeyboardShortcut("l", modifiers: [.command, .option])) } == true)
        #expect(keymap.conflict(KeyboardShortcut("d", modifiers: [.command, .shift]), for: .find) == nil)

        KeyboardShortcuts.setShortcut(nil, for: name)
        keymap.reload()
        #expect(keymap.help("Dictate, on this device", .dictate) == "Dictate, on this device")
        #expect(keymap.active(.dictate) == nil)
        #expect(keymap.columns.flatMap { $0 }.flatMap(\.rows).contains(KeyGroup.Row(title: "Dictate", key: nil)))

        KeyboardShortcuts.reset(name)
        keymap.reload()
        #expect(keymap.label(.dictate) == "⇧⌘D")
    }
    #endif
}
