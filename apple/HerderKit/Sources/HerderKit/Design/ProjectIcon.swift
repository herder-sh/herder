import Foundation
import SwiftUI

#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// A project's icon file, when this app has one, and the colour its machine fills the tile
/// with, as `#rrggbb`.
struct ProjectIconImage: Equatable {
    var data: Data?
    var background: String?
}

/// A project's small rounded tile: its own icon when there is one, else its initial; on the
/// background its machine sets, else on a colour that stays the same for the project
/// everywhere.
struct ProjectIcon: View {
    /// `nil` for sessions whose project is not known yet.
    let projectId: String?
    /// The name the initial comes from; the id's last component when not given.
    var name: String?
    /// The project's icon image, when known.
    var image: ProjectIconImage?
    var size: CGFloat = 20

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: size * 0.27, style: .continuous)
        Group {
            if let image, let picture = image.data.flatMap(Self.platformImage) {
                picture.resizable().interpolation(.high).scaledToFill()
                    .background(image.background.flatMap(Self.colour) ?? .clear)
            } else if let projectId, let rgb = image?.background.flatMap(Self.rgb) {
                Text(Self.initial(name ?? projectId))
                    .font(.system(size: size * 0.56, weight: .bold, design: .rounded))
                    .foregroundStyle(Self.isLight(rgb) ? Color(light: 0x1A1A1A, dark: 0x1A1A1A) : .white)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(Color(light: rgb, dark: rgb))
            } else if let projectId {
                let tint = Self.tint(projectId)
                Text(Self.initial(name ?? projectId))
                    .font(.system(size: size * 0.56, weight: .bold, design: .rounded))
                    .foregroundStyle(tint)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(tint.opacity(0.2))
            } else {
                Image(systemName: "questionmark")
                    .font(.system(size: size * 0.5, weight: .bold))
                    .foregroundStyle(Theme.tertiary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(Theme.raised)
            }
        }
        .frame(width: size, height: size)
        .clipShape(shape)
        .overlay(shape.strokeBorder(Theme.text.opacity(0.08)))
        .accessibilityHidden(true)
    }

    /// The tile colours: the dark palette's status hues, and greys in light mode.
    static let tints: [Color] = [
        Color(light: 0x3D3D3B, dark: 0x56B4E9),
        Color(light: 0x5A5A58, dark: 0x009E73),
        Color(light: 0x2E7D7A, dark: 0xE69F00),
        Color(light: 0x4A4A48, dark: 0xD55E00),
        Color(light: 0x6A6A68, dark: 0xCC79A7),
        Color(light: 0x2B2B2A, dark: 0xF0E442),
        Color(light: 0x52524F, dark: 0x0072B2),
    ]

    /// Which tint a project gets: from an FNV-1a hash of its id, so it is the same on every
    /// launch and device (Swift's own hashes change per launch).
    static func tintIndex(_ projectId: String) -> Int {
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        for byte in projectId.utf8 {
            hash ^= UInt64(byte)
            hash = hash &* 0x0000_0100_0000_01b3
        }
        return Int(hash % UInt64(tints.count))
    }

    static func tint(_ projectId: String) -> Color { tints[tintIndex(projectId)] }

    /// The first letter or digit of the name's last path component, upper-cased.
    static func initial(_ name: String) -> String {
        let last = name.split(whereSeparator: { $0 == "/" || $0 == ":" }).last.map(String.init) ?? name
        return last.first { $0.isLetter || $0.isNumber }.map { String($0).uppercased() } ?? "#"
    }

    /// The colour `#rrggbb` names, if it is one.
    static func colour(_ hex: String) -> Color? {
        guard let rgb = rgb(hex) else { return nil }
        return Color(light: rgb, dark: rgb)
    }

    /// The `0xrrggbb` of `#rrggbb`, if it is one.
    static func rgb(_ hex: String) -> UInt32? {
        let digits = hex.dropFirst()
        guard hex.first == "#", digits.count == 6, digits.allSatisfy(\.isHexDigit) else { return nil }
        return UInt32(digits, radix: 16)
    }

    /// Whether dark text reads better than white on `rgb`, by its relative luminance.
    static func isLight(_ rgb: UInt32) -> Bool {
        let red = Double(rgb >> 16 & 0xFF), green = Double(rgb >> 8 & 0xFF), blue = Double(rgb & 0xFF)
        return (0.2126 * red + 0.7152 * green + 0.0722 * blue) / 255 > 0.5
    }

    private static func platformImage(_ data: Data) -> Image? {
        #if os(iOS)
        UIImage(data: data).map { Image(uiImage: $0) }
        #else
        NSImage(data: data).map { Image(nsImage: $0) }
        #endif
    }
}
