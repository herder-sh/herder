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
    /// The canvas: the first of the three surfaces, under every page.
    static let background = Color(light: 0xE6E6E4, dark: 0x0A0A0A)
    /// Sidebar, sheets, composer. A section on a page has no box of its own: it is a
    /// `SectionHeading` over rows split by hairlines.
    static let surface = Color(light: 0xEDEDEB, dark: 0x141414)
    /// The raised surface: a row's hover and selection, a settings group, chips, code blocks,
    /// secondary buttons.
    static let raised = Color(light: 0xDADAD8, dark: 0x1A1A1A)
    static let stroke = Color(light: 0xC9C9C7, dark: 0x252525)

    static let text = Color(light: 0x1C1C1C, dark: 0xF7F7F7)
    static let secondary = Color(light: 0x6A6A6A, dark: 0xD1D1D1)
    static let tertiary = Color(light: 0x8C8C8A, dark: 0x939393)

    /// The primary button: a dark raised fill with a lighter border, never white
    /// (`primaryFill(in:)`).
    static let primaryFill = Color(light: 0xD0D0CE, dark: 0x26262B)
    static let primaryStroke = Color(light: 0xA8A8A6, dark: 0x3D3D44)
    static let bubble = Color(light: 0x333333, dark: 0x1A1A1A)
    static let onBubble = Color(light: 0xECECEA, dark: 0xF7F7F7)

    /// Needs you: approvals, questions, anything waiting on the user, and nothing else.
    static let accent = Color(light: 0x2E7D7A, dark: 0xE69F00)
    static let running = Color(light: 0x1C1C1C, dark: 0x5382EE)
    static let waiting = Color(light: 0x8C8C8A, dark: 0x999999)
    static let idle = Color(light: 0xB6B6B4, dark: 0x666666)
    static let failure = Color(light: 0x5A5A58, dark: 0xD55E00)
    static let success = Color(light: 0x1C1C1C, dark: 0x009E73)
    static let merged = Color(light: 0x5A5A58, dark: 0xCC79A7)
    /// Links in prose, which are underlined too, as light mode has no colour to spare.
    static let link = Color(light: 0x1C1C1C, dark: 0x7FA6F5)
    /// Marks a child session: its banner, header edge and composer.
    static let child = Color(light: 0x3C3C3A, dark: 0x9D8CF0)

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
    func primaryFill<S: InsettableShape>(in shape: S, on: Bool = true, off: Color = Theme.raised) -> some View {
        background(on ? Theme.primaryFill : off, in: shape)
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
            .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
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
