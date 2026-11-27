import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct ShortcutActionsTests {
    /// The demo fleet with its lists in.
    private func demo() async throws -> (Fleet, Task<Void, Never>) {
        let fleet = try Fleet.demo()
        let following = Task { await fleet.follow() }
        #expect(await eventually(within: 10) { !fleet.lists.projects.isEmpty })
        return (fleet, following)
    }

    /// Shortcuts lists each known project once, and a search narrows them by name.
    @Test func projectsAreListedAndFoundByName() async throws {
        let (fleet, following) = try await demo()
        defer { following.cancel() }
        let projects = ShortcutActions.projects(in: fleet)
        #expect(!projects.isEmpty)
        #expect(projects.map(\.id) == fleet.lists.projects.compactMap(\.projectId))
        #expect(Set(projects.map(\.id)).count == projects.count)
        let first = try #require(projects.first)
        let found = ShortcutActions.projects(in: fleet, matching: first.name.uppercased())
        #expect(found.contains(first))
        #expect(ShortcutActions.projects(in: fleet, matching: "no project is called this").isEmpty)
    }

    /// Sessions waiting for you come first, none archived, each with a link that opens it.
    @Test func sessionsWaitingForYouComeFirst() async throws {
        let (fleet, following) = try await demo()
        defer { following.cancel() }
        let sessions = ShortcutActions.sessions(in: fleet)
        #expect(!sessions.isEmpty)
        let waiting = sessions.prefix { $0.needsYou }
        #expect(!sessions.dropFirst(waiting.count).contains { $0.needsYou })
        #expect(Set(sessions.map(\.id)).count == sessions.count)
        for session in sessions {
            guard case .session(let key)? = URL(string: session.id).flatMap(AppLink.init(url:)) else {
                Issue.record("\(session.id) is not a session link")
                continue
            }
            #expect(fleet.session(key)?.state != .archived)
        }
        let first = try #require(sessions.first)
        #expect(ShortcutActions.sessions(in: fleet, matching: first.title).contains(first))
    }

    /// A session's id is the link `herder://session` opens.
    @Test func aSessionsLinkReadsBack() throws {
        let key = SessionKey(hostId: "studio", sessionId: "s 1&2")
        #expect(AppLink(url: try #require(URL(string: ShortcutActions.link(key)))) == .session(key))
    }

    /// An empty prompt starts nothing.
    @Test func anEmptyPromptIsRefused() async throws {
        let fleet = try Fleet.demo()
        let draft = try #require(Draft.chat(fleet: fleet))
        await #expect(throws: ShortcutActions.Problem.self) {
            try await ShortcutActions.start(draft, prompt: "  \n", in: fleet)
        }
        #expect(fleet.starting.isEmpty)
    }

    /// Opening a project or session goes where the window's links and sidebar go.
    @Test func openingRoutesToTheWindow() throws {
        let fleet = try Fleet.demo()
        let key = SessionKey(hostId: "studio", sessionId: "1")
        #expect(CommandRoute(.openProject("p"), fleet: fleet, item: .board) == .section(.project("p")))
        #expect(CommandRoute(.openSession(key), fleet: fleet, item: .board) == .session(key))
    }
}
