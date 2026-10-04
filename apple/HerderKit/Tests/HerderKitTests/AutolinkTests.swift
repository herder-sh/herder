import Foundation
import Testing
@testable import HerderKit

@MainActor
struct AutolinkTests {
    private func links(_ text: String) -> [String] {
        MarkdownText.inline(text).runs.compactMap { $0.link?.absoluteString }
    }

    @Test func aBareURLBecomesALink() {
        let string = MarkdownText.inline("Open as PR **#194**: https://github.com/herder-sh/herder/pull/194.")
        let run = string.runs.first { $0.link != nil }
        #expect(run?.link == URL(string: "https://github.com/herder-sh/herder/pull/194"))
        #expect(run.map { String(string[$0.range].characters) } == "https://github.com/herder-sh/herder/pull/194")
    }

    @Test func codeSpansExplicitLinksAndSchemelessNamesStayAsTheyAre() {
        #expect(links("Run `curl https://example.com/x` first").isEmpty)
        #expect(links("See [the PR](https://example.com/a)") == ["https://example.com/a"])
        #expect(links("Edit main.rs or box.tailnet.ts.net").isEmpty)
    }
}
