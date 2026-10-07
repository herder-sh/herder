import Foundation
import Herder
@testable import HerderKit
import Testing

private func item(_ id: String, _ body: ItemBody, turn: String = "t1") -> EventBody {
    .itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: id, turnId: turn, body: body))
}

struct HtmlVisualTests {
    @Test func showHtmlCallsGetTheirOwnCardBetweenTheQuietToolRuns() {
        var script = Script()
        let model = script.model([
            created(), .turnStarted(turnId: "t1"),
            item("c1", .toolCall(name: "Bash", input: #"{"command":"ls"}"#)),
            item("c2", .toolCall(name: "mcp__herder__show_html", input: #"{"title":"Sales","html":"<p>chart</p>"}"#)),
            item("r2", .toolResult(callId: "c2", output: "Shown.", isError: false)),
            item("c3", .toolCall(name: "Read", input: #"{"file_path":"a.rs"}"#)),
            .turnCompleted(turnId: "t1", usage: nil),
        ])
        let blocks = Transcript.blocks(model)
        guard blocks.count == 3, case .tools(_, let before) = blocks[0], case .visual(let visual) = blocks[1],
              case .tools(_, let after) = blocks[2] else {
            Issue.record("unexpected blocks: \(blocks)")
            return
        }
        #expect(before.map(\.name) == ["Bash"])
        #expect(after.map(\.name) == ["Read"])
        #expect(visual.title == "Sales")
        #expect(visual.html == "<p>chart</p>")
        #expect(!visual.building)
    }

    @Test func recognisesEachProvidersNaming() {
        for name in ["mcp__herder__show_html", "herder.show_html", "herder-show_html", "show_html"] {
            #expect(HtmlVisual.isShowHtml(name), "\(name)")
        }
        for name in ["Bash", "show_html_later", "mcp__herder__spawn", "reshow_html"] {
            #expect(!HtmlVisual.isShowHtml(name), "\(name)")
        }
    }

    @Test func waitsForTheWholeInputAndAPage() {
        let streaming = HtmlVisual(id: "v", input: #"{"title":"Sales","html":"<p>ch"#, streaming: true)
        #expect(streaming.html == nil)
        #expect(streaming.building)
        let empty = HtmlVisual(id: "v", input: #"{"title":"  "}"#, streaming: false)
        #expect(empty.html == nil)
        #expect(!empty.building)
        #expect(empty.title == "Visual")
    }

    @Test func thePolicyComesFirstAfterTheDoctype() {
        let page = HtmlVisual.sandboxed("\n<!DOCTYPE html><html><body>hi</body></html>")
        #expect(page.hasPrefix("<!DOCTYPE html>\n<meta http-equiv=\"Content-Security-Policy\""))
        #expect(page.hasSuffix("<html><body>hi</body></html>"))
        #expect(HtmlVisual.sandboxed("<p>hi</p>").hasPrefix("<meta http-equiv=\"Content-Security-Policy\""))
        #expect(HtmlVisual.policy.hasPrefix("default-src 'none';"))
    }
}
