import SwiftUI

/// Text whose links show the pointing hand on hover. Selectable text keeps the I-beam over its
/// links, so the links are found in the laid-out text and the pointer set while over one.
struct LinkText: View {
    let string: AttributedString
    @State private var links: [CGRect] = []
    @State private var overLink = false

    init(_ string: AttributedString) { self.string = string }

    var body: some View {
        Self.text(string)
            .backgroundPreferenceValue(Text.LayoutKey.self) { layouts in
                GeometryReader { proxy in
                    let rects = Self.links(in: layouts) { proxy[$0] }
                    Color.clear
                        .onAppear { links = rects }
                        .onChange(of: rects) { links = rects }
                }
            }
            .onContinuousHover { phase in
                if case .active(let point) = phase {
                    overLink = links.contains { $0.contains(point) }
                } else {
                    overLink = false
                }
            }
            .pointerStyle(overLink ? .link : nil)
    }

    private struct Link: TextAttribute {}

    /// `string` as one text, its links marked so the layout can find them.
    static func text(_ string: AttributedString) -> Text {
        string.runs[\.link].reduce(Text(verbatim: "")) { text, run in
            let piece = Text(AttributedString(string[run.1]))
            return Text("\(text)\(run.0 == nil ? piece : piece.customAttribute(Link()))")
        }
    }

    /// Where the links of `text(_:)` sit, given where each layout's origin resolves to.
    static func links(in layouts: [Text.LayoutKey.AnchoredLayout], resolve: (Anchor<CGPoint>) -> CGPoint) -> [CGRect] {
        layouts.flatMap { anchored in
            let origin = resolve(anchored.origin)
            return anchored.layout.flatMap { line in
                line.filter { $0[Link.self] != nil }
                    .map { $0.typographicBounds.rect.offsetBy(dx: origin.x, dy: origin.y) }
            }
        }
    }
}
