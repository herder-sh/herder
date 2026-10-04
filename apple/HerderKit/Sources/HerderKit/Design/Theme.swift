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
    static let background = Color(light: 0xE6E6E4, dark: 0x0A0A0A)
    static let surface = Color(light: 0xEDEDEB, dark: 0x141414)
    /// Code blocks, chips, secondary buttons.
    static let raised = Color(light: 0xDADAD8, dark: 0x1A1A1A)
    static let stroke = Color(light: 0xC9C9C7, dark: 0x252525)

    static let text = Color(light: 0x1C1C1C, dark: 0xF7F7F7)
    static let secondary = Color(light: 0x6A6A6A, dark: 0xD1D1D1)
    static let tertiary = Color(light: 0x8C8C8A, dark: 0x939393)

    /// The primary button: ink on paper.
    static let primary = Color(light: 0x1C1C1C, dark: 0xF7F7F7)
    static let onPrimary = Color(light: 0xE6E6E4, dark: 0x141414)
    static let bubble = Color(light: 0x333333, dark: 0x1A1A1A)
    static let onBubble = Color(light: 0xECECEA, dark: 0xF7F7F7)

    /// Attention: approvals, questions, anything waiting on the user.
    static let accent = Color(light: 0x2E7D7A, dark: 0xE69F00)
    static let running = Color(light: 0x1C1C1C, dark: 0x5382EE)
    static let waiting = Color(light: 0x8C8C8A, dark: 0x999999)
    static let idle = Color(light: 0xB6B6B4, dark: 0x666666)
    static let failure = Color(light: 0x5A5A58, dark: 0xD55E00)
    static let success = Color(light: 0x1C1C1C, dark: 0x009E73)
    static let merged = Color(light: 0x5A5A58, dark: 0xCC79A7)
    /// Marks a child session: its banner, header edge and composer.
    static let child = Color(light: 0x3C3C3A, dark: 0x9D8CF0)

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
