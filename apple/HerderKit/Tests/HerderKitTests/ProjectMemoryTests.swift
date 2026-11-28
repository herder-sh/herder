import Foundation
import Herder
@testable import HerderKit
import Testing

private func summary(_ id: String, _ state: SessionState = .idle) -> SessionSummary {
    SessionSummary(key: SessionKey(hostId: "h", sessionId: id), title: id, project: "app", branch: "",
                   machine: "h", state: state, activity: "", age: "")
}

private func project(_ projectId: String?, live: [SessionSummary], archived: [SessionSummary] = []) -> ProjectGroup {
    ProjectGroup(projectId: projectId, name: projectId ?? "No project yet", live: live, archived: archived)
}

struct ProjectMemoryTests {
    /// A store of its own, so tests neither share nor leave state.
    let memory = ProjectMemory(defaults: UserDefaults(suiteName: "ProjectMemoryTests-\(UUID().uuidString)")!)

    @Test func remembersTheLastSessionOpenInEachProject() {
        let app = project("acme/app", live: [summary("a"), summary("b")])
        let api = project("acme/api", live: [summary("c")])
        #expect(memory.last(in: app) == nil)
        memory.remember(SessionKey(hostId: "h", sessionId: "a"), in: app)
        memory.remember(SessionKey(hostId: "h", sessionId: "b"), in: app)
        memory.remember(SessionKey(hostId: "h", sessionId: "c"), in: api)
        #expect(memory.last(in: app) == SessionKey(hostId: "h", sessionId: "b"))
        #expect(memory.last(in: api) == SessionKey(hostId: "h", sessionId: "c"))
    }

    @Test func survivesANewStoreOnTheSameDefaults() {
        let defaults = UserDefaults(suiteName: "ProjectMemoryTests-\(UUID().uuidString)")!
        let app = project("acme/app", live: [summary("a")])
        ProjectMemory(defaults: defaults).remember(SessionKey(hostId: "h", sessionId: "a"), in: app)
        #expect(ProjectMemory(defaults: defaults).last(in: app) == SessionKey(hostId: "h", sessionId: "a"))
    }

    @Test func doesNotRememberASessionTheProjectDoesNotList() {
        let app = project("acme/app", live: [summary("a")])
        memory.remember(SessionKey(hostId: "h", sessionId: "elsewhere"), in: app)
        #expect(memory.last(in: app) == nil)
        // Nor for the sessions no project holds yet.
        let none = project(nil, live: [summary("x")])
        memory.remember(SessionKey(hostId: "h", sessionId: "x"), in: none)
        #expect(memory.last(in: none) == nil)
    }

    @Test func fallsBackOnceTheSessionIsArchivedOrGone() {
        let key = SessionKey(hostId: "h", sessionId: "a")
        memory.remember(key, in: project("acme/app", live: [summary("a")]))
        // Archived: listed under Archived, or leading live children in the live list.
        #expect(memory.last(in: project("acme/app", live: [], archived: [summary("a", .archived)])) == nil)
        #expect(memory.last(in: project("acme/app", live: [summary("a", .archived), summary("b")])) == nil)
        // Gone from its machine.
        #expect(memory.last(in: project("acme/app", live: [summary("b")])) == nil)
        // Back again, it opens again.
        #expect(memory.last(in: project("acme/app", live: [summary("a")])) == key)
    }

    @Test func remembersOnlyWhatIsPushedOverAProjectsScreen() {
        let a = SessionKey(hostId: "h", sessionId: "a")
        let b = SessionKey(hostId: "h", sessionId: "b")
        let shown = ProjectMemory.shown(in: [.project("acme/app"), .session(a)])
        #expect(shown?.projectId == "acme/app" && shown?.session == a)
        // A child pushed over its parent is the one open.
        #expect(ProjectMemory.shown(in: [.project("acme/app"), .session(a), .session(b)])?.session == b)
        // A session found by the Projects list's search is not opened inside its project.
        #expect(ProjectMemory.shown(in: [.session(a)]) == nil)
        #expect(ProjectMemory.shown(in: [.project("acme/app")]) == nil)
    }

    @Test func reopensOnlyOnEnteringAProjectNotOnComingBackToIt() {
        let a = SessionKey(hostId: "h", sessionId: "a")
        #expect(ProjectMemory.entered(from: [], to: [.project("acme/app")]) == "acme/app")
        #expect(ProjectMemory.entered(from: [.project("acme/api"), .session(a)], to: [.project("acme/app")]) == "acme/app")
        // Back from the session to the list stays on the list.
        #expect(ProjectMemory.entered(from: [.project("acme/app"), .session(a)], to: [.project("acme/app")]) == nil)
        #expect(ProjectMemory.entered(from: [], to: [.project("acme/app"), .session(a)]) == nil)
    }
}
