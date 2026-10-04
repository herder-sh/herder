import Testing
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
