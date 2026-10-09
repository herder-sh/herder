import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct PromptDraftsTests {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent("drafts-\(UUID().uuidString)", isDirectory: true)
    let drafts: PromptDrafts

    init() { drafts = PromptDrafts(directory: directory) }

    @Test func aDraftIsKeptPerProjectWithItsImages() {
        let image = Herder.Image(mediaType: "image/png", data: Data([1, 2, 3]))
        drafts.save(.init(text: "fix [Image #1]", images: [image]), for: "github.com/acme/app")
        drafts.save(.init(text: "other", images: []), for: "/Users/me/repo")
        let kept = drafts.load("github.com/acme/app")
        #expect(kept?.text == "fix [Image #1]")
        #expect(kept?.herderImages == [image])
        #expect(drafts.load("/Users/me/repo")?.text == "other")
        #expect(drafts.load("github.com/acme/web") == nil)
    }

    @Test func aDraftKeepsItsFilesAndIsKeptForThemAlone() {
        let file = PromptFile(name: "report.xlsx", data: Data([4, 5]))
        drafts.save(.init(text: "", images: [], files: [file]), for: "p")
        #expect(drafts.load("p")?.promptFiles == [file])
        drafts.save(.init(text: "", images: []), for: "p")
        #expect(drafts.load("p") == nil)
    }

    @Test func anEmptiedDraftIsForgotten() {
        drafts.save(.init(text: "half a thought", images: []), for: "p")
        drafts.save(.init(text: "  \n", images: []), for: "p")
        #expect(drafts.load("p") == nil)
    }

    @Test func eachSessionKeepsItsOwnDraft() {
        let one = SessionKey(hostId: "mac", sessionId: "s1")
        let two = SessionKey(hostId: "mac", sessionId: "s2")
        drafts.save(.init(text: "for one", images: []), for: PromptDrafts.key(one))
        drafts.save(.init(text: "for two", images: []), for: PromptDrafts.key(two))
        #expect(drafts.load(PromptDrafts.key(one))?.text == "for one")
        #expect(drafts.load(PromptDrafts.key(two))?.text == "for two")
        #expect(drafts.load(PromptDrafts.key(SessionKey(hostId: "linux", sessionId: "s1"))) == nil)
    }

    @Test func aPromptMovesWithItsDraftToAnotherProject() {
        drafts.save(.init(text: "fix the login", images: []), for: "a")
        drafts.move(.init(text: "fix the login", images: []), from: "a", to: "b")
        #expect(drafts.load("a") == nil)
        #expect(drafts.load("b")?.text == "fix the login")
    }

    @Test func movingAnEmptyPromptKeepsTheOtherProjectsDraft() {
        drafts.save(.init(text: "kept", images: []), for: "b")
        drafts.move(.init(text: "", images: []), from: "a", to: "b")
        #expect(drafts.load("b")?.text == "kept")
    }

    @Test func newSessionDraftsStayListedTheLastWrittenFirstUntilEmptied() {
        let app = Draft(hostId: "mac", projectId: "app")
        let web = Draft(hostId: "linux", projectId: "web")
        drafts.save(.init(text: "fix the login", images: [], draft: app), for: app.key)
        drafts.save(.init(text: "for a session", images: []), for: PromptDrafts.key(SessionKey(hostId: "mac", sessionId: "s1")))
        drafts.save(.init(text: "add dark mode", images: [], draft: web), for: web.key)
        #expect(drafts.unsent.map(\.draft) == [web, app])
        #expect(PromptDrafts(directory: directory).unsent.map(\.draft) == [web, app])
        drafts.save(.init(text: "", images: [], draft: web), for: web.key)
        #expect(drafts.unsent.map(\.draft) == [app])
    }

    @Test func aDroppedDraftIsNotKeptAgainUntilOpenedAnew() {
        let app = Draft(hostId: "mac", projectId: "app")
        drafts.save(.init(text: "fix the login", images: [], draft: app), for: app.key)
        drafts.drop(app)
        // Leaving the open draft keeps what it shows; dropped, it stays gone.
        drafts.save(.init(text: "fix the login", images: [], draft: app), for: app.key)
        #expect(drafts.unsent.isEmpty)
        #expect(drafts.load(app.key) == nil)
        drafts.save(.init(text: "start over", images: [], draft: app), for: app.key)
        #expect(drafts.load(app.key)?.text == "start over")
    }
}
