import SwiftUI

/// The one control of every page, pane and session header: an icon, a label where the header
/// has room, `HeaderControl.height` high and at least as wide, in one quiet borderless style.
/// Only its state tints it: `tint` for what it shows (a PR's state, a job needing you), filled
/// while its mode is on, a raised fill under the pointer.
enum HeaderControl {
    static let height: CGFloat = 32
    /// The gap between a header's controls.
    static let spacing: CGFloat = 4
}

/// A header button: `HeaderLabel` as a plain button. Its caller adds the tooltip.
struct HeaderButton: View {
    let symbol: String
    var title: String?
    var tint: Color = Theme.secondary
    var selected = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HeaderLabel(symbol: symbol, title: title, tint: tint, selected: selected)
        }
        .buttonStyle(.plain)
    }
}

/// What a header button, menu or link shows: `symbol` and `title` in the header control's style.
struct HeaderLabel: View {
    let symbol: String
    var title: String?
    var tint: Color = Theme.secondary
    var selected = false

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: symbol)
            if let title { Text(title).lineLimit(1) }
        }
        .headerControl(tint: tint, selected: selected, labelled: title != nil)
    }
}

extension View {
    /// Draws a label in the header control's style: `HeaderLabel`'s, for a label whose icon is
    /// no symbol (the Activity chip's ring).
    func headerControl(tint: Color = Theme.secondary, selected: Bool = false, labelled: Bool) -> some View {
        modifier(HeaderControlStyle(tint: tint, selected: selected, labelled: labelled))
    }
}

private struct HeaderControlStyle: ViewModifier {
    let tint: Color
    let selected: Bool
    let labelled: Bool
    @State private var hovered = false

    func body(content: Content) -> some View {
        content
            .font(Theme.body.weight(.semibold).monospacedDigit())
            .foregroundStyle(selected || (hovered && tint == Theme.secondary) ? Theme.text : tint)
            .padding(.horizontal, labelled ? 10 : 0)
            .frame(minWidth: HeaderControl.height, minHeight: HeaderControl.height, maxHeight: HeaderControl.height)
            .background(selected || hovered ? Theme.raised : .clear, in: .rect(cornerRadius: 8))
            .fixedSize()
            .onHover { hovered = $0 }
            .hitTarget()
    }
}

/// A menu whose label is a `HeaderLabel`: no indicator, no button chrome. A toolbar's menu
/// keeps the system's look (`applies` false).
struct HeaderMenuStyle: ViewModifier {
    var applies = true

    func body(content: Content) -> some View {
        if applies {
            content
                .menuStyle(.button)
                .buttonStyle(.plain)
                .menuIndicator(.hidden)
                .fixedSize()
        } else {
            content
        }
    }
}

extension VerticalAlignment {
    private enum TitleLine: AlignmentID {
        static func defaultValue(in context: ViewDimensions) -> CGFloat { context[VerticalAlignment.center] }
    }

    /// The centre of a header title's first line, where its controls centre (`titleLine()`).
    static let titleLine = VerticalAlignment(TitleLine.self)
}

extension View {
    /// Marks this text, a header's title, as the line its header's controls centre on: the middle
    /// of its first line, found from its first baseline as the system font's ascender and
    /// descender (0.95 and 0.24 em) place it, so it holds at every Dynamic Type size.
    func titleLine() -> some View {
        alignmentGuide(.titleLine) { $0[.firstTextBaseline] * (1 + 0.24 / 0.95) / 2 }
    }
}
