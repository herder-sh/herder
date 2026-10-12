import SwiftUI

/// Content wider than the transcript, a table or a block of code, scrolled sideways in a box of
/// its own. Its scroll bar shows, and an edge with more content past it fades out, so a table
/// scrolled away from its first column reads as scrolled rather than clipped.
struct SidewaysScroll<Content: View>: View {
    @ViewBuilder let content: Content
    @State private var edges = SidewaysEdges()

    var body: some View {
        ScrollView(.horizontal) { content }
            .scrollIndicators(.visible)
            .onScrollGeometryChange(for: SidewaysEdges.self) { geometry in
                SidewaysEdges(offset: geometry.contentOffset.x, visible: geometry.containerSize.width,
                              content: geometry.contentSize.width)
            } action: { _, new in
                edges = new
            }
            .mask {
                HStack(spacing: 0) {
                    LinearGradient(colors: [.black.opacity(edges.leading ? 0 : 1), .black], startPoint: .leading, endPoint: .trailing)
                        .frame(width: SidewaysEdges.fade)
                    Color.black
                    LinearGradient(colors: [.black, .black.opacity(edges.trailing ? 0 : 1)], startPoint: .leading, endPoint: .trailing)
                        .frame(width: SidewaysEdges.fade)
                }
            }
    }
}

/// Which edges of a sideways scroll have more content past them.
struct SidewaysEdges: Equatable {
    static let fade: CGFloat = 24

    var leading = false
    var trailing = false

    init() {}

    init(offset: CGFloat, visible: CGFloat, content: CGFloat) {
        // A point's slack, as an offset settles a fraction off its bounds.
        leading = offset > 1
        trailing = offset + visible < content - 1
    }
}
