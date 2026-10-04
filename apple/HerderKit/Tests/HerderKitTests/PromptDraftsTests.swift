import Foundation
import Herder
@testable import HerderKit
import Testing

struct PromptDraftsTests {
    let drafts = PromptDrafts(directory: FileManager.default.temporaryDirectory
        .appendingPathComponent("drafts-\(UUID().uuidString)", isDirectory: true))

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

    @Test func anEmptiedDraftIsForgotten() {
        drafts.save(.init(text: "half a thought", images: []), for: "p")
        drafts.save(.init(text: "  \n", images: []), for: "p")
        #expect(drafts.load("p") == nil)
    }
}
