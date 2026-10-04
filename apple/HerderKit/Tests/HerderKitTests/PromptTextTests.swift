import Foundation
import Testing
#if os(macOS)
import AppKit
#endif
@testable import HerderKit

struct PromptTextTests {
    @Test func markersAreFoundInOrder() {
        let tokens = PromptText.tokens(in: "see [Image #1] and [Pasted text #1], then [Image #2]").map(\.token)
        #expect(tokens == [
            PromptText.Token(kind: .image, number: 1), PromptText.Token(kind: .paste, number: 1),
            PromptText.Token(kind: .image, number: 2),
        ])
    }

    @Test func pastesAreSentInPlaceOfTheirMarkers() {
        let text = "fix [Pasted text #1] like [Pasted text #2] [Image #1]"
        #expect(PromptText.expand(text, pastes: ["a\nb", "c"]) == "fix a\nb like c [Image #1]")
    }

    @Test func removingAnItemRenumbersTheMarkersAfterIt() {
        var images = ["a", "b", "c"]
        var text = "[Image #1] x [Image #2] y [Image #3]"
        PromptText.remove(.image, at: 0, items: &images, text: &text)
        #expect(images == ["b", "c"])
        #expect(text == "x [Image #1] y [Image #2]")
    }

    @Test func aDeletedChipTakesItsItemWithIt() {
        var pastes = ["one", "two"]
        var text = "keep [Pasted text #2] only"
        PromptText.prune(.paste, items: &pastes, text: &text)
        #expect(pastes == ["two"])
        #expect(text == "keep [Pasted text #1] only")
    }

    @Test func onlyLongPastesBecomeChips() {
        #expect(!PromptText.isLong("a short line"))
        #expect(PromptText.isLong(String(repeating: "x", count: 1000)))
        #expect(PromptText.isLong(String(repeating: "line\n", count: 15)))
    }
}

#if os(macOS)

struct PastedImageTests {
    @Test func anImageOnThePasteboardCanBePastedAndRead() throws {
        let board = NSPasteboard(name: NSPasteboard.Name("herder-test-\(UUID().uuidString)"))
        defer { board.releaseGlobally() }
        #expect(!ImageAttachment.available(board))
        board.clearContents()
        board.setString("text", forType: .string)
        #expect(!ImageAttachment.available(board))
        let picture = NSImage(size: NSSize(width: 8, height: 8), flipped: false) { rect in
            NSColor.red.setFill(); rect.fill(); return true
        }
        let tiff = try #require(picture.tiffRepresentation)
        let png = try #require(NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]))
        board.clearContents()
        board.setData(png, forType: .png)
        #expect(ImageAttachment.available(board))
        #expect(ImageAttachment.from(board).map(\.mediaType) == ["image/png"])
    }
}
#endif
