import Herder
@testable import HerderKit
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
