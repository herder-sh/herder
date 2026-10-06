#if os(macOS)
import AppKit
import SwiftUI
import Testing
@testable import HerderKit

@MainActor struct LinkTextTests {
    private final class Found { var links: [CGRect] = [] }

    private func links(_ markdown: String) -> [CGRect] {
        let found = Found()
        let view = LinkText.text(MarkdownText.inline(markdown))
            .backgroundPreferenceValue(Text.LayoutKey.self) { layouts in
                GeometryReader { proxy in
                    Color.clear.onAppear { found.links = LinkText.links(in: layouts) { proxy[$0] } }
                }
            }
            .frame(width: 400)
        let host = NSHostingView(rootView: view)
        host.frame = CGRect(origin: .zero, size: host.fittingSize)
        host.layoutSubtreeIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.1))
        return found.links
    }

    @Test func aLinkIsFoundWhereItIsLaidOut() throws {
        let found = links("See PR https://github.com/herder-sh/herder/pull/293 for more")
        #expect(found.count == 1)
        let link = try #require(found.first)
        #expect(link.minX > 20, "the link starts after the words before it, \(found)")
        #expect(link.width > 100, "\(found)")
    }

    @Test func textWithoutLinksHasNone() {
        #expect(links("No links here").isEmpty)
    }
}
#endif
