import Foundation
import Herder
import SwiftUI
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
        #expect(run.map { String(string[$0.range].characters) } == "github.com/herder-sh/herder/pull/194")
        #expect(String(string.characters) == "Open as PR #194: github.com/herder-sh/herder/pull/194.")
    }

    @Test func aBareURLShowsItsHostAndTheStartOfItsPath() throws {
        func shown(_ url: String) throws -> String { MarkdownText.display(try #require(URL(string: url))) }
        #expect(try shown("https://auth.planetscale.com/oauth/device?user_code=SZJ3Q1WD") == "auth.planetscale.com/oauth/device…")
        #expect(try shown("https://www.example.com/") == "example.com")
        #expect(try shown("http://localhost:8080/a/b#top") == "localhost:8080/a/b…")
        #expect(try shown("https://example.com/one/two/three/four/five/six/seven") == "example.com/one/two/three/four/five/six…")
        #expect(try shown("https://example.com/caf%C3%A9") == "example.com/café")
    }

    @Test func aMarkdownLinkKeepsItsText() {
        let string = MarkdownText.inline("See [the device page](https://auth.planetscale.com/oauth/device?user_code=X)")
        #expect(String(string.characters) == "See the device page")
    }

    @Test func severalBareURLsAreEachShortened() {
        let string = MarkdownText.inline("https://a.com/x?y=1 and https://b.com/z?w=2")
        #expect(String(string.characters) == "a.com/x… and b.com/z…")
        #expect(links("https://a.com/x?y=1 and https://b.com/z?w=2") == ["https://a.com/x?y=1", "https://b.com/z?w=2"])
    }

    @Test func codeSpansExplicitLinksAndSchemelessNamesStayAsTheyAre() {
        #expect(links("Run `curl https://example.com/x` first").isEmpty)
        #expect(links("See [the PR](https://example.com/a)") == ["https://example.com/a"])
        #expect(links("Edit main.rs or box.tailnet.ts.net").isEmpty)
    }
}

@MainActor
struct PullRequestLinkTests {
    private let prs = PRLinks([PullRequest(number: 251, url: "https://github.com/herder-sh/herder/pull/251", title: "Archive",
                                           headBranch: nil, state: .open, ci: .passing, review: .none, mergeable: .clean)])

    private func links(_ text: String) -> [String] {
        MarkdownText.inline(text, prs: prs).runs.compactMap { $0.link?.absoluteString }
    }

    @Test func aPullRequestReferenceLinksToThePullRequestOrItsRepository() {
        #expect(links("Opened #251.") == ["https://github.com/herder-sh/herder/pull/251"])
        #expect(links("See PR #12, then (#13)") == ["https://github.com/herder-sh/herder/pull/12",
                                                    "https://github.com/herder-sh/herder/pull/13"])
    }

    @Test func codeAnchorsAndUnknownRepositoriesStayText() {
        #expect(links("Run `git show #251`, see page#12 or &#38;").isEmpty)
        #expect(MarkdownText.inline("Opened #251").runs.allSatisfy { $0.link == nil })
    }

    @Test func linksAreUnderlined() {
        let string = MarkdownText.inline("Opened #251 at https://example.com", prs: prs)
        #expect(string.runs.filter { $0.link != nil }.allSatisfy { $0.underlineStyle != nil })
    }
}
