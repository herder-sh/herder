import CoreGraphics
@testable import HerderKit
import Testing

struct TranscriptLogTests {
    /// A view `height` tall, 600 pt by default, on 5,000 pt of transcript, at `offset`.
    private func state(offset: CGFloat, content: CGFloat = 5000, height: CGFloat = 600) -> TranscriptScrollState {
        TranscriptScrollState(contentHeight: content, offsetY: offset,
                              visible: CGRect(x: 0, y: offset, width: 800, height: height),
                              container: CGSize(width: 800, height: height))
    }

    @Test func aViewWithinTheContentIsInside() {
        #expect(state(offset: 0).placement == .inside)
        #expect(state(offset: 2000).placement == .inside)
        #expect(state(offset: 4400).placement == .inside)
        // Rounding in the scroll view's own arithmetic is not overscroll.
        #expect(state(offset: 4400.6).placement == .inside)
    }

    @Test func scrolledPastEitherEdgeIsSaid() {
        #expect(state(offset: -40).placement == .aboveTop)
        #expect(state(offset: 4500).placement == .pastEnd)
    }

    @Test func aViewShowingNoneOfTheContentIsOutside() {
        #expect(state(offset: 5000).placement == .outside)
        #expect(state(offset: 9000).placement == .outside)
        #expect(state(offset: -600).placement == .outside)
        // The content shrank under a view that stayed where it was.
        #expect(state(offset: 4400, content: 1200).placement == .outside)
    }

    @Test func insetsWidenTheRange() {
        let state = TranscriptScrollState(contentHeight: 5000, offsetY: 4420,
                                          visible: CGRect(x: 0, y: 4420, width: 800, height: 600),
                                          container: CGSize(width: 800, height: 600), insetTop: 0, insetBottom: 20)
        #expect(state.placement == .inside)
    }

    @Test func aContentShorterThanTheViewRestsAtItsTop() {
        #expect(state(offset: 0, content: 300).placement == .inside)
        #expect(state(offset: 10, content: 300).placement == .pastEnd)
    }

    /// What the owner's Mac logged as its transcript went blank: the content shrank by a
    /// thousand points while the offset stayed, past the new end of a window's 900 pt view.
    @Test func contentShrinkingFromUnderTheViewIsCaught() {
        let before = state(offset: 1675, content: 3446, height: 900)
        #expect(state(offset: 1675, content: 2298, height: 900).shrankFromUnder(before))
        #expect(state(offset: 4400, content: 1200).shrankFromUnder(state(offset: 4400)))
    }

    @Test func aBounceOrAShrinkThatLeavesTheViewInsideIsLeftAlone() {
        // A bounce at the end, on content that did not shrink.
        #expect(!state(offset: 4500).shrankFromUnder(state(offset: 4400)))
        // Content that shrank above a view still within it.
        #expect(!state(offset: 1000, content: 4000).shrankFromUnder(state(offset: 1000)))
        // Content that grew.
        #expect(!state(offset: 4400, content: 6000).shrankFromUnder(state(offset: 4400)))
    }

    @Test func noBlockInViewOfATranscriptWithBlocksIsBlank() {
        #expect(TranscriptScrollState.blank(visible: 0, blocks: 120))
        #expect(!TranscriptScrollState.blank(visible: 3, blocks: 120))
        #expect(!TranscriptScrollState.blank(visible: 0, blocks: 0))
    }
}
