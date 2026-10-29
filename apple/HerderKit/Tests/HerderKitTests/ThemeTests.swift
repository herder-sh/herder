#if os(macOS)
import AppKit
@testable import HerderKit
import SwiftUI
import Testing

/// The dark theme's elevation scale: each layer is lighter than the one it sits on, and none
/// darker than the canvas.
struct ThemeTests {
    @Test func eachDarkLayerIsLighterThanItsParent() throws {
        let scale = try [Theme.canvas, Theme.pane, Theme.card, Theme.sheet, Theme.cardInSheet].map(dark)
        #expect(scale[0] >= 0x16 / 255.0 - 0.001)
        for (parent, child) in zip(scale, scale.dropFirst()) {
            #expect(child > parent)
        }
        #expect(try dark(Theme.raised) > dark(Theme.cardInSheet))
    }

    @Test func aCardTakesItsColourFromItsLayer() {
        #expect(Layer.canvas.card == Theme.card)
        #expect(Layer.pane.card == Theme.card)
        #expect(Layer.sheet.card == Theme.cardInSheet)
        #expect(Layer.pane.ground == Theme.pane)
    }

    /// The colour's luminance in the dark appearance.
    private func dark(_ color: Color) throws -> Double {
        var resolved: NSColor?
        try #require(NSAppearance(named: .darkAqua)).performAsCurrentDrawingAppearance {
            resolved = NSColor(color).usingColorSpace(.sRGB)
        }
        let rgb = try #require(resolved)
        return 0.2126 * rgb.redComponent + 0.7152 * rgb.greenComponent + 0.0722 * rgb.blueComponent
    }
}
#endif
