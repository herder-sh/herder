import Testing
@testable import HerderKit

struct SidewaysScrollTests {
    /// A table scrolled away from its first column fades its leading edge, so it reads as
    /// scrolled rather than clipped; one that fits fades neither.
    @Test func anEdgeFadesWhereContentGoesOnPastIt() {
        #expect(SidewaysEdges(offset: 0, visible: 500, content: 900) == edges(leading: false, trailing: true))
        #expect(SidewaysEdges(offset: 200, visible: 500, content: 900) == edges(leading: true, trailing: true))
        #expect(SidewaysEdges(offset: 400, visible: 500, content: 900) == edges(leading: true, trailing: false))
        #expect(SidewaysEdges(offset: 0, visible: 500, content: 300) == edges(leading: false, trailing: false))
        #expect(SidewaysEdges(offset: 0.4, visible: 500, content: 500.5) == edges(leading: false, trailing: false))
    }

    private func edges(leading: Bool, trailing: Bool) -> SidewaysEdges {
        var edges = SidewaysEdges()
        edges.leading = leading
        edges.trailing = trailing
        return edges
    }
}
