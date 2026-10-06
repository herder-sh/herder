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
        #expect(run.map { String(string[$0.range].characters) } == "https://github.com/herder-sh/herder/pull/194")
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
