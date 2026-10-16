import Foundation
import Herder
@testable import HerderKit
import Testing

struct TranscriptWindowTests {
    private let key = SessionKey(hostId: "h", sessionId: "01A")
    private let other = SessionKey(hostId: "h", sessionId: "01B")
    private let blocks = (0..<100).map { TranscriptBlock.assistant(id: "b\($0)", text: "", streaming: false) }

    @Test func drawsOnlyTheLastPageAtFirst() {
        let window = TranscriptWindow()
        #expect(window.shown(blocks, of: key).map(\.id) == (88..<100).map { "b\($0)" })
        #expect(window.hides(blocks, of: key))
        #expect(!window.hides(Array(blocks.prefix(TranscriptWindow.page)), of: key))
    }

    @Test func scrollingNearTheTopOfWhatIsDrawnDrawsAPageMore() {
        var window = TranscriptWindow()
        // The view's top is well inside what is drawn: nothing more.
        let inside = window.scrolled(to: "b95", in: blocks, of: key)
        #expect(!inside)
        let nearTop = window.scrolled(to: "b89", in: blocks, of: key)
        #expect(nearTop)
        #expect(window.shown(blocks, of: key).first?.id == "b76")
        // Each session keeps its own.
        #expect(window.shown(blocks, of: other).first?.id == "b88")
    }

    @Test func aJumpDrawsDownToItsBlockAndLeavingGoesBackToOnePage() {
        var window = TranscriptWindow()
        window.reveal("b10", in: blocks, of: key)
        #expect(window.shown(blocks, of: key).first?.id == "b10")
        window.reveal("b95", in: blocks, of: key)
        #expect(window.shown(blocks, of: key).first?.id == "b10")
        window.reset(key)
        #expect(window.shown(blocks, of: key).count == TranscriptWindow.page)
    }
}
