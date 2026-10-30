@testable import HerderKit
import Foundation
import SwiftUI
import Testing

struct MessageTextTests {
    let picture = MessagePicture(number: 1, mediaType: "image/png", size: 11_000, data: nil, attachmentId: "a1")

    @Test func anImageMarkerWithAnImageBehindItIsMarkedAsItsChip() {
        let string = MessageText.marked(AttributedString("[Image #1] also this\nlike [Image #2]"), pictures: [picture])
        let chips = string.runs.compactMap { run in run[PictureChip.self].map { (String(string[run.range].characters), $0) } }
        #expect(chips.count == 1)
        #expect(chips.first?.0 == "[Image #1]")
        #expect(chips.first?.1 == picture)
    }

    @MainActor @Test func aChipIsOneCharacterThatCopiesAsItsMarker() {
        var string = MessageText.marked(AttributedString("see [Image #1] here"), pictures: [picture])
        // Find's marks split the marker's runs; it stays one chip.
        let split = string.characters.index(string.startIndex, offsetBy: 8)
        string[split..<string.characters.index(after: split)].backgroundColor = Color.yellow
        let text = TranscriptText.attributed(string, style: .body, monospaced: false, lineSpacing: 0, color: .white)
        #expect(text.length == "see ".count + 1 + " here".count)
        #expect(text.containsAttachments(in: NSRange(location: 0, length: text.length)))
        #expect(text.copied(NSRange(location: 0, length: text.length)) == "see [Image #1] here")
        #expect(text.copied(NSRange(location: 4, length: 3)) == "[Image #1] h")
        #if os(macOS)
        // Not a link on a Mac, where a drag from a link selects only the link.
        #expect(text.attribute(.link, at: 4, effectiveRange: nil) == nil)
        #expect(text.attribute(.chipLink, at: 4, effectiveRange: nil) as? URL == PictureChip.url(picture))
        #else
        #expect(text.attribute(.link, at: 4, effectiveRange: nil) as? URL == PictureChip.url(picture))
        #endif
        #expect(PictureChip.url(picture).flatMap(PictureChip.number) == 1)
    }

    @Test func anotherLinkIsNoPicture() {
        #expect(PictureChip.number(URL(string: "https://herder.sh/1")!) == nil)
    }

    @Test func onlyAMarkerWithAnImageBehindItDrawsAsChips() {
        #expect(MessageText.refers("see [Image #1]", to: [picture]))
        #expect(!MessageText.refers("see [Image #2]", to: [picture]))
        #expect(!MessageText.refers("see [Image #1]", to: []))
    }

    @Test func aPictureIsNamedByItsNumberAndType() {
        #expect(picture.name == "image.png")
        #expect(MessagePicture(number: 2, mediaType: "image/jpeg", size: 0, data: nil, attachmentId: nil).name == "image-2.jpg")
    }
}
