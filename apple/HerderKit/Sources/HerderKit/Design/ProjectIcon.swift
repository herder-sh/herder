import Foundation
import SwiftUI

#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// A project's small rounded tile: its own icon when there is one, else its initial on a
/// colour that stays the same for the project everywhere.
struct ProjectIcon: View {
    /// `nil` for sessions whose project is not known yet.
    let projectId: String?
    /// The name the initial comes from; the id's last component when not given.
    var name: String?
    /// The project's icon image, when known.
    var image: Data?
    var size: CGFloat = 20

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: size * 0.27, style: .continuous)
        Group {
            if let image = image.flatMap(Self.platformImage) {
                image.resizable().interpolation(.high).scaledToFill()
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

    private static func platformImage(_ data: Data) -> Image? {
        #if os(iOS)
        UIImage(data: data).map { Image(uiImage: $0) }
        #else
        NSImage(data: data).map { Image(nsImage: $0) }
        #endif
    }
}
