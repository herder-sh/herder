import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct CommandsTests {
    /// A command waits for the window to take it, once.
    @Test func theWindowTakesASentCommandOnce() {
        let commands = AppCommands()
        commands.send(.newChat)
        #expect(commands.pending == .newChat)
        #expect(commands.take() == .newChat)
        #expect(commands.take() == nil)
    }

    /// Speak to Chat's key stops the dictation running rather than opening another chat.
    @Test func speakToChatAgainStopsDictating() {
        let commands = AppCommands()
        let composer = UUID()
        var stopped = 0
        commands.dictation(composer, stop: { stopped += 1 })
        commands.send(.speakToChat)
        #expect(stopped == 1)
        #expect(commands.pending == nil)

        commands.dictation(composer, stop: nil)
        commands.send(.speakToChat)
        #expect(stopped == 1)
        #expect(commands.pending == .speakToChat)
    }

    /// A composer that goes leaves the stop of another one dictating since alone.
    @Test func anotherComposerEndingKeepsTheDictationsStop() {
        let commands = AppCommands()
        var stopped = false
        commands.dictation(UUID(), stop: { stopped = true })
        commands.dictation(UUID(), stop: nil)
        commands.send(.speakToChat)
        #expect(stopped)
    }

    /// Only the chat Speak to Chat opened starts listening, and only once.
    @Test func theOpenedChatListensOnce() {
        let commands = AppCommands()
        commands.listen(for: Draft.chatKey)
        #expect(!commands.takeListening(for: nil))
        #expect(!commands.takeListening(for: "project"))
        #expect(commands.takeListening(for: Draft.chatKey))
        #expect(!commands.takeListening(for: Draft.chatKey))
    }

    /// Speak to Chat opens a chat on a connected machine that runs sessions, listening; New
    /// Chat the same chat, not listening.
    @Test func speakToChatOpensAListeningChat() throws {
        let fleet = try Fleet.demo()
        guard case .draft(let draft, let listens) = CommandRoute(.speakToChat, fleet: fleet, item: .board) else {
            Issue.record("Speak to Chat opened no draft")
            return
        }
        #expect(listens)
        #expect(draft.isChat)
        let machine = try #require(fleet.machines.first { $0.hostId == draft.hostId })
        #expect(machine.connection == .connected && machine.hosts.isEmpty)
        #expect(CommandRoute(.newChat, fleet: fleet, item: .board) == .draft(draft, listens: false))
    }

    /// ⌃⌥⌘D with a session or a draft on show dictates into its composer; only with none
    /// does it open a chat. New Chat opens one either way.
    @Test func speakToChatDictatesIntoTheComposerOnShow() throws {
        let fleet = try Fleet.demo()
        #expect(CommandRoute(.speakToChat, fleet: fleet, item: .board, composing: true) == .dictate)
        guard case .draft(_, true) = CommandRoute(.speakToChat, fleet: fleet, item: .board, composing: false) else {
            Issue.record("Speak to Chat with nothing open opened no listening chat")
            return
        }
        guard case .draft(_, false) = CommandRoute(.newChat, fleet: fleet, item: .board, composing: true) else {
            Issue.record("New Chat over a session opened no chat")
            return
        }
    }

    /// The composer that came on show last is the one that dictates; once it goes, the one
    /// under it; once all have gone, none.
    @Test func theComposerOnShowLastDictates() {
        let commands = AppCommands()
        let session = UUID(), draft = UUID()
        var toggled: [String] = []
        #expect(!commands.composing)
        commands.composer(session) { toggled.append("session") }
        commands.composer(draft) { toggled.append("draft") }
        #expect(commands.composing)
        commands.dictateInComposer()
        commands.composer(draft, toggle: nil)
        commands.dictateInComposer()
        commands.composer(session, toggle: nil)
        commands.dictateInComposer()
        #expect(toggled == ["draft", "session"])
        #expect(!commands.composing)
    }

    /// With no machine to chat on, it says so instead of opening nothing.
    @Test func withoutAMachineAChatSaysWhy() throws {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("the profile did not open")
            return
        }
        #expect(CommandRoute(.speakToChat, fleet: fleet, item: .board) == .problem("No machine is connected to chat on"))
        #expect(CommandRoute(.newSession, fleet: fleet, item: .board) == .sheet(.newSession))
    }

    /// ⌘N is a chat in Chats, a draft in the project shown, the picker elsewhere.
    @Test func newSessionStartsWhereTheWindowIs() async throws {
        let fleet = try Fleet.demo()
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        #expect(CommandRoute(.newSession, fleet: fleet, item: .board) == .sheet(.newSession))
        guard case .draft(let chat, false) = CommandRoute(.newSession, fleet: fleet, item: .chats) else {
            Issue.record("⌘N in Chats opened no chat")
            return
        }
        #expect(chat.isChat)

        #expect(await eventually(within: 10) { !fleet.lists.projects.isEmpty })
        let project = try #require(fleet.lists.projects.first { $0.projectId != nil })
        guard case .draft(let draft, false) = CommandRoute(.newSession, fleet: fleet, item: .project(project.id)) else {
            Issue.record("⌘N in a project opened no draft")
            return
        }
        #expect(draft.projectId == project.projectId)
    }

    /// ⌘1… go down the sidebar, each on a key of its own.
    @Test func eachSectionHasItsOwnKey() throws {
        let fleet = try Fleet.demo()
        let sections = AppCommand.Section.allCases
        #expect(sections.map(\.key) == ["1", "2", "3", "4", "5", "6"])
        #expect(sections.map(\.title) == ["Board", "Chats", "Bots", "Providers", "Skills", "Hub"])
        #expect(CommandRoute(.go(.bots), fleet: fleet, item: .board) == .section(.bots(nil)))
        #expect(CommandRoute(.go(.hub), fleet: fleet, item: .chats) == .section(.hub))
        #expect(CommandRoute(.reconnect, fleet: fleet, item: .board) == .reconnect)
    }
}
