import Foundation
import Herder
@testable import HerderKit
import Testing

private func item(_ id: String, _ body: ItemBody, turn: String = "t1") -> EventBody {
    .itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: id, turnId: turn, body: body))
}

private func published(_ name: String, url: String? = "https://krowk.com/a/art_1",
                       expiresAt: String? = "2026-10-11T17:57:01.233Z") -> EventBody {
    .artifactPublished(title: "Settings screen",
                       attachment: Attachment(attachmentId: "01ART", mediaType: "application/octet-stream", size: 3,
                                              name: name),
                       url: url, expiresAt: expiresAt)
}

struct ArtifactTests {
    @Test func aPublishedArtifactGetsItsOwnCardBetweenTheQuietToolRuns() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            item("c1", .toolCall(name: "Bash", input: #"{"command":"ls"}"#)),
            item("c2", .toolCall(name: "mcp__herder__publish", input: #"{"title":"Settings screen","path":"s.png"}"#)),
            published("settings.png"),
            item("r2", .toolResult(callId: "c2", output: #"{"url":"https://krowk.com/a/art_1"}"#, isError: false)),
            item("c3", .toolCall(name: "Read", input: #"{"file_path":"a.rs"}"#)),
            .turnCompleted(turnId: "t1", usage: nil),
        ])
        let blocks = Transcript.blocks(model)
        guard blocks.count == 3, case .tools(_, let before) = blocks[0], case .artifact(let artifact) = blocks[1],
              case .tools(_, let after) = blocks[2] else {
            Issue.record("unexpected blocks: \(blocks)")
            return
        }
        #expect(before.map(\.name) == ["Bash", "mcp__herder__publish"])
        #expect(after.map(\.name) == ["Read"])
        #expect(artifact.title == "Settings screen")
        #expect(artifact.kind == .image)
        #expect(artifact.url == URL(string: "https://krowk.com/a/art_1"))
        // It stays in view once the turn's work folds away.
        #expect(Transcript.collapsed(model).contains { if case .artifact = $0 { true } else { false } })
    }

    @Test func rendersByTheFileNamesExtension() {
        let kinds: [(String, Artifact.Kind)] = [
            ("shot.PNG", .image), ("flow.mov", .video), ("report.html", .page), ("test.log", .text),
            ("diff.patch", .text), ("bundle.zip", .file),
        ]
        for (name, kind) in kinds {
            guard case .artifactPublished(let title, let attachment, let url, let expiresAt) = published(name) else { continue }
            let artifact = Artifact(id: 1, title: title, attachment: attachment, url: url, expiresAt: expiresAt)
            #expect(artifact.kind == kind, "\(name)")
        }
    }

    @Test func offersTheLinkUntilItExpires() {
        let attachment = Attachment(attachmentId: "a", mediaType: "application/octet-stream", size: 1, name: "a.png")
        let live = Artifact(id: 1, title: "t", attachment: attachment, url: "https://krowk.com/a/1",
                            expiresAt: "2026-10-11T12:00:00Z")
        let before = Timestamp.date("2026-10-11T11:59:00Z")!
        let after = Timestamp.date("2026-10-11T12:00:01Z")!
        #expect(live.link(at: before) == URL(string: "https://krowk.com/a/1"))
        #expect(live.link(at: after) == nil)
        let forever = Artifact(id: 2, title: "t", attachment: attachment, url: "https://krowk.com/a/2", expiresAt: nil)
        #expect(forever.link(at: after) != nil)
        let privately = Artifact(id: 3, title: "t", attachment: attachment, url: nil, expiresAt: nil)
        #expect(privately.link(at: before) == nil)
    }

    @Test func thePolicyComesFirstAfterTheDoctype() {
        let page = SandboxedPage.wrapped("\n<!DOCTYPE html><html><body>hi</body></html>")
        #expect(page.hasPrefix("<!DOCTYPE html>\n<meta http-equiv=\"Content-Security-Policy\""))
        #expect(page.hasSuffix("<html><body>hi</body></html>"))
        #expect(SandboxedPage.wrapped("<p>hi</p>").hasPrefix("<meta http-equiv=\"Content-Security-Policy\""))
        #expect(SandboxedPage.policy.hasPrefix("default-src 'none';"))
    }
}
