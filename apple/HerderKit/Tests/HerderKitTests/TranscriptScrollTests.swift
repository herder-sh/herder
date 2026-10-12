import Herder
@testable import HerderKit
import SwiftUI
import Testing

struct TranscriptScrollTests {
    let parent = SessionKey(hostId: "h", sessionId: "01P")
    let child = SessionKey(hostId: "h", sessionId: "01C")

    @Test func aSessionLeftScrolledUpLandsWhereItWasOnce() {
        var scroll = TranscriptScroll()
        scroll.saw("b30", in: parent)
        scroll.scrolled(parent, atEnd: false)
        scroll.saw("c1", in: child)
        scroll.scrolled(child, atEnd: true)
        scroll.show(parent)
        // The new transcript reports itself at its end before it appears.
        scroll.scrolled(parent, atEnd: true)
        #expect(scroll.land() == "b30")
        // Landed once: later appearances, and new output, are left alone.
        #expect(scroll.land() == nil)
    }

    @Test func aSessionLeftAtItsEndOpensAtItsEndAndFollows() {
        var scroll = TranscriptScroll()
        scroll.saw("b30", in: parent)
        scroll.scrolled(parent, atEnd: false)
        scroll.saw("b52", in: parent)
        scroll.scrolled(parent, atEnd: true)
        scroll.show(child)
        #expect(scroll.land() == nil)
        scroll.show(parent)
        #expect(scroll.land() == nil)
    }

    @Test func eachSessionKeepsItsOwnPlace() {
        var scroll = TranscriptScroll()
        scroll.saw("b30", in: parent)
        scroll.scrolled(parent, atEnd: false)
        scroll.saw("c4", in: child)
        scroll.scrolled(child, atEnd: false)
        scroll.show(child)
        #expect(scroll.land() == "c4")
        scroll.show(parent)
        #expect(scroll.land() == "b30")
    }
}

struct FoldSpotTests {
    let viewport = CGRect(x: 0, y: -600, width: 400, height: 800)

    @Test func theFoldedLineTakesTheClosingLinesPlaceOnScreen() {
        // A line 3/4 down the view, in its own space with the view's bounds converted to it.
        let line = CGRect(x: 0, y: -10, width: 80, height: 20)
        #expect(FoldSpot.anchor(of: line, in: viewport) == UnitPoint(x: 0, y: 0.75))
    }

    @Test func aLineOutOfSightLandsAtTheViewsEdge() {
        #expect(FoldSpot.anchor(of: CGRect(x: 0, y: 900, width: 80, height: 20), in: viewport).y == 1)
        #expect(FoldSpot.anchor(of: CGRect(x: 0, y: -2000, width: 80, height: 20), in: viewport).y == 0)
        #expect(FoldSpot.anchor(of: .zero, in: .zero) == .bottom)
    }
}
