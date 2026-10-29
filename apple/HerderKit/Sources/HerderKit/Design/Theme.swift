import SwiftUI

#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// herder's palette and type. Light is e-ink: grey paper, pure greyscale, one muted teal spent
/// on "needs you". Dark follows the T3 Nightly reference: soft charcoal surfaces,
/// neutral chat bubbles, off-white text, and blue activity accents.
enum Theme {
    /// The elevation scale: each layer is lighter than the one it sits on, in dark mode as in
    /// light. Views paint the layer they are on with `.ground` and a card on it with `.card`
    /// (`Layer`), rather than these.
    ///
    /// The canvas: under a page, a session and the empty detail pane.
    static let canvas = Color(light: 0xE6E6E4, dark: 0x161618)
    /// The sidebar and a list pane, one step from the canvas beside them.
    static let pane = Color(light: 0xEDEDEB, dark: 0x1B1B1E)
    /// A card, a settings group, the composer: on the canvas or a pane.
    static let card = Color(light: 0xF4F4F2, dark: 0x222226)
    /// A sheet and a popover.
    static let sheet = Color(light: 0xEDEDEB, dark: 0x26262A)
    /// A card, a settings group or a field inside a sheet.
    static let cardInSheet = Color(light: 0xF7F7F5, dark: 0x2E2E33)
    /// A row's hover and selection, chips, code blocks, secondary buttons: above any card.
    static let raised = Color(light: 0xDADAD8, dark: 0x313136)
    /// Where two equal layers meet, and a card's edge.
    static let stroke = Color(light: 0xC9C9C7, dark: 0x34343A)

    static let text = Color(light: 0x1C1C1C, dark: 0xF7F7F7)
    static let secondary = Color(light: 0x6A6A6A, dark: 0xD1D1D1)
    static let tertiary = Color(light: 0x8C8C8A, dark: 0x939393)

    /// The primary button: a dark raised fill with a lighter border, never white
    /// (`primaryFill(in:)`).
    static let primaryFill = Color(light: 0xD0D0CE, dark: 0x26262B)
    static let primaryStroke = Color(light: 0xA8A8A6, dark: 0x3D3D44)
    static let bubble = Color(light: 0x333333, dark: 0x2A2A2F)
    static let onBubble = Color(light: 0xECECEA, dark: 0xF7F7F7)
    /// The ground of inline code in a bubble.
    static let onBubbleCode = onBubble.opacity(0.14)

    /// Needs you: approvals, questions, anything waiting on the user, and nothing else.
    static let accent = Color(light: 0x2E7D7A, dark: 0xE69F00)
    static let running = Color(light: 0x1C1C1C, dark: 0x5382EE)
    static let waiting = Color(light: 0x8C8C8A, dark: 0x999999)
    static let idle = Color(light: 0xB6B6B4, dark: 0x666666)
    static let failure = Color(light: 0x5A5A58, dark: 0xD55E00)
    static let success = Color(light: 0x1C1C1C, dark: 0x009E73)
    static let merged = Color(light: 0x5A5A58, dark: 0xCC79A7)
    /// A plan limit running low: not amber, as it needs no answer yet.
    static let limit = Color(light: 0x8E4A75, dark: 0xCC79A7)
    /// Links in prose, which are underlined too, as light mode has no colour to spare.
    static let link = Color(light: 0x1C1C1C, dark: 0x7FA6F5)
    /// Marks a child session: its banner, header edge and composer.
    static let child = Color(light: 0x3C3C3A, dark: 0x9D8CF0)

    /// Settings pages (P16.28). A switch that is on, and the dot of a page's "Connected" chip.
    static let switchOn = Color(light: 0x2BB673, dark: 0x3ECF8E)
    /// A switch that is off: a clearly dark track under its white knob.
    static let switchOff = Color(light: 0xC2C2C8, dark: 0x303036)
    /// The primary button of a settings sheet, "+ Add …" rows and editable values.
    static let action = Color(light: 0x3D6FE0, dark: 0x5B8CFF)
    /// What removes or forgets something; on its own page only, never in the section index.
    static let danger = Color(light: 0xD4483A, dark: 0xEF6B5B)
    /// The fills of a person's initials (`Avatar`), white on each in both appearances.
    static let avatars: [Color] = [0x6A5ACD, 0x2F8F83, 0xB5653A, 0x3D7FC1, 0xA8508F, 0x7A8A35]
        .map { Color(light: $0, dark: $0) }

    /// The type scale: a page's title, a section or row title, body text, and meta (times,
    /// counts, hints). The Mac uses the scale's point sizes; iOS the Dynamic Type styles
    /// nearest them.
    #if os(macOS)
    static let page = Font.system(size: 20, weight: .bold)
    static let title = Font.system(size: 14, weight: .semibold)
    static let body = Font.system(size: 13)
    static let meta = Font.system(size: 11.5)
    #else
    static let page = Font.title2.weight(.bold)
    static let title = Font.headline
    static let body = Font.subheadline
    static let meta = Font.footnote
    #endif

    /// Monospace is only for versions, branches, ids and code.
    static let mono = Font.system(.footnote, design: .monospaced)
    static let monoSmall = Font.system(.caption, design: .monospaced)

    static let corner: CGFloat = 10

    /// The layout grid every page follows. A page's header and content start `margin` from its
    /// edges (`paneMargin` in a list pane beside a session; `pageMargin` reads the one in use),
    /// its content at most `pageWidth` wide from the leading edge, so it lines up with the title.
    #if os(macOS)
    static let margin: CGFloat = 24
    #else
    static let margin: CGFloat = 16
    #endif
    static let paneMargin: CGFloat = 16
    static let pageWidth: CGFloat = 920
    /// A settings page's column (`DetailPage`), centred in the room beside its index.
    static let settingsWidth: CGFloat = 880
    /// A page header's height, the sidebar's top bar's on the Mac, so their lines meet.
    static let headerHeight: CGFloat = 52
    /// A section heading's line (TODAY, ACCOUNTS), over its rows.
    static let sectionHeight: CGFloat = 32
    /// A row of one line, and a row with a second line under its title. iOS rows grow from them
    /// with Dynamic Type.
    static let rowHeight: CGFloat = 44
    static let tallRowHeight: CGFloat = 52
    /// A cell's inset from its column's edge, in tables and grouped rows alike.
    static let cellPadding: CGFloat = 14
}

/// The layer a view sits on: the canvas, a pane or a sheet. `layer(_:)` paints its ground and
/// sets it for what it holds, so a card picks its colour from its container.
enum Layer {
    case canvas, pane, sheet

    var ground: Color {
        switch self {
        case .canvas: Theme.canvas
        case .pane: Theme.pane
        case .sheet: Theme.sheet
        }
    }

    var card: Color { self == .sheet ? Theme.cardInSheet : Theme.card }
}

/// The ground of the layer in use, as a fill (`.ground`).
struct GroundStyle: ShapeStyle {
    func resolve(in environment: EnvironmentValues) -> Color { environment.layer.ground }
}

/// A card's fill on the layer in use (`.card`).
struct CardStyle: ShapeStyle {
    func resolve(in environment: EnvironmentValues) -> Color { environment.layer.card }
}

extension ShapeStyle where Self == GroundStyle {
    static var ground: GroundStyle { GroundStyle() }
}

extension ShapeStyle where Self == CardStyle {
    static var card: CardStyle { CardStyle() }
}

extension View {
    /// Puts this on `layer`: paints its ground behind it and sets it for the cards inside. A
    /// sheet on iOS sets it as its presentation's background too, over the system's glass.
    func layer(_ layer: Layer) -> some View {
        environment(\.layer, layer).background(layer.ground)
    }
}

extension EnvironmentValues {
    @Entry var layer = Layer.canvas
    /// The page margin in use: `Theme.margin`, or `Theme.paneMargin` in a list pane.
    @Entry var pageMargin = Theme.margin
}

extension View {
    /// Lays a page's scrolled content on the grid: the margin in use from its edges, at most
    /// `width` wide, from the leading edge under the page's title.
    func pageColumn(width: CGFloat = Theme.pageWidth) -> some View {
        modifier(PageColumn(width: width))
    }
}

private struct PageColumn: ViewModifier {
    let width: CGFloat
    @Environment(\.pageMargin) private var margin

    func body(content: Content) -> some View {
        content
            .frame(maxWidth: width, alignment: .leading)
            .padding(.horizontal, margin)
            .padding(.top, 8)
            .padding(.bottom, 24)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

extension Color {
    /// A colour that follows the appearance: `light` in light mode, `dark` in dark mode.
    init(light: UInt32, dark: UInt32) {
        #if os(iOS)
        self.init(uiColor: UIColor { traits in
            UIColor(rgb: traits.userInterfaceStyle == .dark ? dark : light)
        })
        #else
        self.init(nsColor: NSColor(name: nil) { appearance in
            NSColor(rgb: appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? dark : light)
        })
        #endif
    }
}

#if os(iOS)
private extension UIColor {
    convenience init(rgb: UInt32) {
        self.init(
            red: CGFloat((rgb >> 16) & 0xFF) / 255, green: CGFloat((rgb >> 8) & 0xFF) / 255,
            blue: CGFloat(rgb & 0xFF) / 255, alpha: 1)
    }
}
#else
private extension NSColor {
    convenience init(rgb: UInt32) {
        self.init(
            srgbRed: CGFloat((rgb >> 16) & 0xFF) / 255, green: CGFloat((rgb >> 8) & 0xFF) / 255,
            blue: CGFloat(rgb & 0xFF) / 255, alpha: 1)
    }
}
#endif

extension View {
    /// The primary button's look on `shape`: the dark raised fill with its lighter border. With
    /// `on` false it takes the plain `off` fill instead, for a control that is primary only
    /// while it is selected. The label stays `Theme.text`, semibold.
    func primaryFill<S: InsettableShape>(in shape: S, on: Bool = true, off: AnyShapeStyle = AnyShapeStyle(Theme.raised)) -> some View {
        background(on ? AnyShapeStyle(Theme.primaryFill) : off, in: shape)
            .overlay(shape.strokeBorder(on ? Theme.primaryStroke : .clear))
    }
}

/// A rounded surface card.
struct Card<Content: View>: View {
    var padding: CGFloat = 14
    @ViewBuilder var content: Content

    var body: some View {
        content
            .padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.card, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// A small capsule label: model, account, machine, mode.
struct Chip: View {
    var symbol: String?
    let text: String
    var tint: Color = Theme.secondary

    var body: some View {
        HStack(spacing: 4) {
            if let symbol { Image(systemName: symbol).imageScale(.small) }
            Text(text).lineLimit(1)
        }
        .font(.caption.weight(.medium))
        .foregroundStyle(tint)
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(Theme.raised, in: .capsule)
    }
}

/// A section heading with an optional count.
struct SectionHeading: View {
    let title: String
    var count: Int?
    var tint: Color = Theme.secondary

    var body: some View {
        HStack(spacing: 6) {
            Text(title.uppercased())
            if let count { Text("\(count)").foregroundStyle(Theme.tertiary) }
        }
        .font(.caption.weight(.semibold))
        .tracking(0.6)
        .foregroundStyle(tint)
    }
}
