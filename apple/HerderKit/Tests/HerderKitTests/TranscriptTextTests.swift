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
        .background(Theme.canvas)
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

    /// The selection is the text view's own, so Copy right after selecting copies it: SwiftUI's
    /// selectable text handed its selection over only on its next update, and a quick ⌘C
    /// copied nothing.
    @Test func copyRightAfterSelectingCopiesTheSelection() throws {
        let host = NSHostingView(rootView: TranscriptText(string: AttributedString("Done. The change is on its branch.")))
        host.frame = CGRect(x: 0, y: 0, width: 400, height: 100)
        host.layoutSubtreeIfNeeded()
        let view = try #require(Self.first(NSTextView.self, in: host))
        view.setSelectedRange(NSRange(location: 6, length: 10))
        let board = NSPasteboard.withUniqueName()
        defer { board.releaseGlobally() }
        #expect(view.writeSelection(to: board, types: view.writablePasteboardTypes))
        #expect(board.string(forType: .string) == "The change")
    }

    /// The owner's case: a prompt with a picture in it is one text, which a drag selects across,
    /// copied as the message reads, marker and all. Its words were texts of their own, and a
    /// drag selected one of them.
    @Test func aPromptWithAPictureIsOneTextThatCopiesWhole() throws {
        let picture = MessagePicture(number: 1, mediaType: "image/png", size: 837, data: nil, attachmentId: nil)
        let text = "[Image #1] we are not merging smoking room to non-smoking room"
        let host = NSHostingView(rootView: MessageText(text: text, pictures: [picture]) { _ in })
        host.frame = CGRect(x: 0, y: 0, width: 300, height: 100)
        host.layoutSubtreeIfNeeded()
        let views = Self.all(NSTextView.self, in: host)
        #expect(views.count == 1, "the message is one text view")
        let view = try #require(views.first)
        view.selectAll(nil)
        let saved = NSPasteboard.general.string(forType: .string)
        defer {
            NSPasteboard.general.clearContents()
            if let saved { NSPasteboard.general.setString(saved, forType: .string) }
        }
        view.copy(nil)
        #expect(NSPasteboard.general.string(forType: .string) == text)
    }

    /// A triple click selects the paragraph, which in a prompt is all of it. XCUITest cannot
    /// click three times in a row on a Mac, so this asks the text view what a triple click takes.
    @Test func aTripleClickSelectsAWholePrompt() throws {
        let text = "Carry on here: fix it and run the checkout suite in WebKit until it passes 20 times in a row."
        let host = NSHostingView(rootView: TranscriptText(string: AttributedString(text)))
        host.frame = CGRect(x: 0, y: 0, width: 300, height: 200)
        host.layoutSubtreeIfNeeded()
        let view = try #require(Self.first(NSTextView.self, in: host))
        let range = view.selectionRange(forProposedRange: NSRange(location: 40, length: 0), granularity: .selectByParagraph)
        #expect((view.string as NSString).substring(with: range) == text)
    }

    /// A click on inline code copies exactly its text, and shows "Copied" over it.
    @Test func clickingInlineCodeCopiesIt() throws {
        let host = NSHostingView(rootView: MarkdownText(text: "Set `https://example.com/callback` as the redirect URL."))
        host.frame = CGRect(x: 0, y: 0, width: 500, height: 100)
        host.layoutSubtreeIfNeeded()
        let view = try #require(Self.first(NSTextView.self, in: host))
        let index = (view.string as NSString).range(of: "example").location
        let saved = NSPasteboard.general.string(forType: .string)
        defer {
            NSPasteboard.general.clearContents()
            if let saved { NSPasteboard.general.setString(saved, forType: .string) }
        }
        view.clicked(onLink: try #require(view.textStorage?.attribute(.link, at: index, effectiveRange: nil)), at: index)
        #expect(NSPasteboard.general.string(forType: .string) == "https://example.com/callback")
        #expect(view.subviews.contains { $0.identifier?.rawValue == "copied-badge" })
    }

    /// A list item's wrapped lines hang under its text, past the marker.
    @Test func aListItemHangsItsWrappedLines() throws {
        let prose = MarkdownText(text: "")
        let lines = MarkdownText.proseLines(MarkdownText.parse("- one"))
        let text = TranscriptText.attributed(prose.prose(lines, last: true), style: .body, monospaced: false,
                                             lineSpacing: 3, color: .white)
        let paragraph = try #require(text.attribute(.paragraphStyle, at: 0, effectiveRange: nil) as? NSParagraphStyle)
        #expect(paragraph.headIndent > paragraph.firstLineHeadIndent)
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

    private static func all<View: NSView>(_ type: View.Type, in view: NSView) -> [View] {
        (view as? View).map { [$0] } ?? view.subviews.flatMap { all(type, in: $0) }
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
