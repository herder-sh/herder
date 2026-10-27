import Foundation
import Testing
@testable import HerderKit

@MainActor
struct CodeCopyTests {
    /// The code each span of `markdown` copies, in order.
    private func copied(_ markdown: String) -> [String] {
        let text = TranscriptText.attributed(MarkdownText.inline(markdown), style: .body, monospaced: false,
                                             lineSpacing: 0, color: .white)
        var spans: [String] = []
        var index = 0
        while index < text.length {
            if let span = text.codeSpan(at: index) {
                spans.append(span.code)
                index = NSMaxRange(span.range)
            } else {
                index += 1
            }
        }
        return spans
    }

    @Test func aSpanCopiesExactlyItsText() {
        #expect(copied("Set `https://example.com/oauth/callback?state=a&b=c#top` as the redirect URL.")
                == ["https://example.com/oauth/callback?state=a&b=c#top"])
        #expect(copied("Add `GITHUB_CLIENT_SECRET=value` to `.env`, then run `make secret`.")
                == ["GITHUB_CLIENT_SECRET=value", ".env", "make secret"])
        #expect(copied("Odd ones: `100%`, `a+b`, `  two  spaces`, `//comment`, `ü`")
                == ["100%", "a+b", "  two  spaces", "//comment", "ü"])
    }

    /// A find match inside a span splits it into runs; a click still copies all of it.
    @Test func aSpanSplitByAFindMatchCopiesWhole() {
        var string = MarkdownText.inline("Run `make secret` now")
        FindHighlight(query: "sec", current: false).mark(&string)
        let text = TranscriptText.attributed(string, style: .body, monospaced: false, lineSpacing: 0, color: .white)
        let start = (text.string as NSString).range(of: "make").location
        #expect(text.codeSpan(at: start)?.code == "make secret")
        #expect(text.codeSpan(at: start + 6)?.code == "make secret")
    }

    /// Text, a link, and code that is a link are no copy target: the link opens.
    @Test func onlyPlainCodeIsACopyTarget() {
        #expect(copied("plain [`linked`](https://example.com) https://example.com/x").isEmpty)
    }

    /// The copy link is herder's own, which no other link handler takes for its own.
    @Test func theCopyLinkIsNoOtherLink() throws {
        let url = try #require(CodeCopy.url("herder://session?host=a"))
        #expect(url.scheme == "herder-copy")
        #expect(CodeCopy.code(url) == "herder://session?host=a")
        #expect(AppLink(url: url) == nil)
        #expect(CodeCopy.code(try #require(URL(string: "https://example.com"))) == nil)
    }
}
