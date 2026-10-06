import Testing
@testable import HerderKit

struct MarkdownProseTests {
    @Test func aBlankLineAfterALineIsOneGap() {
        #expect(MarkdownText.parse("Hi,\n\n\nThanks.\n- one\n\n") == [.line("Hi,"), .gap, .line("Thanks."), .line("- one")])
        #expect(MarkdownText.parse("\n\nHi,") == [.line("Hi,")])
    }

    @Test func linesBetweenOtherBlocksAreOneBlockToSelectAcross() {
        let parts = MarkdownText.parse("Hi,\n\nThanks,\nTomas\n```sh\nls\n```\n\nDone\n- one")
        #expect(MarkdownText.blocks(parts) == [
            [.line("Hi,"), .gap, .line("Thanks,"), .line("Tomas")],
            [.code("ls", language: "sh")],
            [.line("Done"), .line("- one")],
        ])
    }
}
