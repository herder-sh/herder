#if os(macOS)
import AppKit
import SwiftUI
import Testing
@testable import HerderKit

@MainActor
struct TranscriptTextTests {
    /// A text taller than the transcript draws its end where the transcript opens, at its end: a
    /// text laying out only the part it first saw left a long reply blank.
    @Test func aTallTextDrawsItsEndWhereTheTranscriptOpens() throws {
        let lines = (1...200).map { "Line \($0) of a reply taller than the transcript." }
        let view = ScrollView {
            TranscriptText(string: AttributedString(lines.joined(separator: "\n")), color: .white)
        }
        .defaultScrollAnchor(.bottom)
        .frame(width: 400, height: 300)
        .background(.black)
        let window = NSWindow(contentRect: CGRect(x: 0, y: 0, width: 400, height: 300), styleMask: .borderless,
                              backing: .buffered, defer: false)
        let host = NSHostingView(rootView: view)
        host.wantsLayer = true
        window.contentView = host
        host.layoutSubtreeIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.2))
        host.layoutSubtreeIfNeeded()

        #expect(try Self.litPixels(host) > 1_000, "the end of the text is drawn")
    }

    /// The pixels of white text on the black background, in the layers as they are on screen:
    /// drawing the view afresh would lay out whatever it is asked to draw.
    private static func litPixels(_ view: NSView) throws -> Int {
        let layer = try #require(view.layer)
        let (width, height) = (Int(view.bounds.width), Int(view.bounds.height))
        let context = try #require(CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                             space: CGColorSpaceCreateDeviceGray(),
                                             bitmapInfo: CGImageAlphaInfo.none.rawValue))
        layer.render(in: context)
        let pixels = try #require(context.data?.assumingMemoryBound(to: UInt8.self))
        return (0..<context.bytesPerRow * height).count { pixels[$0] > 128 }
    }
}
#endif
