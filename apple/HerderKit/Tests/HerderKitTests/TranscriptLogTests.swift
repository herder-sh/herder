import CoreGraphics
@testable import HerderKit
import Testing

struct TranscriptLogTests {
    /// A 600 pt view on 5,000 pt of transcript, at `offset`.
    private func state(offset: CGFloat, content: CGFloat = 5000) -> TranscriptScrollState {
        TranscriptScrollState(contentHeight: content, offsetY: offset,
                              visible: CGRect(x: 0, y: offset, width: 800, height: 600),
                              container: CGSize(width: 800, height: 600))
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

    @Test func noBlockInViewOfATranscriptWithBlocksIsBlank() {
        #expect(TranscriptScrollState.blank(visible: 0, blocks: 120))
        #expect(!TranscriptScrollState.blank(visible: 3, blocks: 120))
        #expect(!TranscriptScrollState.blank(visible: 0, blocks: 0))
    }
}
