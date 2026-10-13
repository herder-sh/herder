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

    /// SwiftUI's text has no paragraph styles, so a gap above a paragraph is a short blank line
    /// and a nested item is indented by tabs, which line up with the tab after a marker.
    @Test func paragraphsKeepTheirSpaceAndIndentsInSwiftUIText() {
        let prose = MarkdownText(text: "")
        let lines = MarkdownText.proseLines(MarkdownText.parse("Intro\n\n- one\n  - nested"))
        let spaced = TranscriptText.spaced(prose.prose(lines, last: true), style: .body, lineSpacing: 3)
        #expect(String(spaced.characters) == "Intro\n\n•\tone\n\t•\tnested")
    }

    /// SwiftUI's text paints a run's ground over its selection, so inline code has none on a
    /// Mac: a selection across it showed dark boxes where the code was.
    @Test func inlineCodeHasNoGroundToHideTheSelection() {
        let prose = MarkdownText(text: "")
        let lines = MarkdownText.proseLines(MarkdownText.parse("Built in `Handoff.swift` as a toast"))
        let spaced = TranscriptText.spaced(prose.prose(lines, last: true), style: .body, lineSpacing: 3)
        let code = spaced.runs.filter { $0.inlinePresentationIntent?.contains(.code) == true }
        #expect(!code.isEmpty)
        #expect(code.allSatisfy { $0.backgroundColor == nil })
    }

    /// A table wider than the transcript scrolls in its own box: the message, its prose and the
    /// table keep to the width they are given, the table starting at its first column.
    @Test func aWideTableKeepsTheMessageToItsWidth() throws {
        let columns = (1...8).map { "Column \($0) with a long heading" }
        let text = "We buy the room and resell it as our own product, fully responsible for delivering it.\n\n"
            + "| " + columns.joined(separator: " | ") + " |\n|" + String(repeating: "---|", count: columns.count) + "\n"
            + "| " + columns.map { _ in "The hotel carries the risk" }.joined(separator: " | ") + " |"
        let host = NSHostingView(rootView: MarkdownText(text: text))
        host.frame = CGRect(x: 0, y: 0, width: 500, height: 400)
        host.layoutSubtreeIfNeeded()

        let table = try #require(Self.first(NSScrollView.self, in: host))
        let frame = table.convert(table.bounds, to: host)
        #expect(frame.minX >= 0, "the table starts at its first column, \(frame)")
        #expect(frame.maxX <= 500, "\(frame)")
    }

    private static func first<View: NSView>(_ type: View.Type, in view: NSView) -> View? {
        if let view = view as? View { return view }
        return view.subviews.lazy.compactMap { first(type, in: $0) }.first
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
