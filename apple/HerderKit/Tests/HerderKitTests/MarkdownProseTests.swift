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

    private func lines(_ text: String) -> [MarkdownText.ProseLine] {
        MarkdownText.proseLines(MarkdownText.parse(text))
    }

    @Test func orderedItemsKeepTheirNumbersAsWritten() {
        #expect(lines("1. one\n2) two\n7. seven") == [
            .init(kind: .item(marker: "1."), text: "one"),
            .init(kind: .item(marker: "2)"), text: "two"),
            .init(kind: .item(marker: "7."), text: "seven"),
        ])
        #expect(lines("- a\n* b\n+ c").map(\.kind) == Array(repeating: .item(marker: "•"), count: 3))
    }

    @Test func notEveryLineStartingWithAMarkIsAnItem() {
        #expect(lines("**bold** text\n-dash\n1.5 times\n---").map(\.kind) == [.text, .text, .text, .text])
    }

    @Test func anIndentedItemNestsOneLevel() {
        #expect(lines("1. one\n   - inner\n     - deeper\n  - inner again\n2. two").map(\.level) == [0, 1, 2, 1, 0])
    }

    @Test func anIndentedLineAfterAnItemGoesOnWithIt() {
        #expect(lines("- one\n  more of one\n    - two\n      more of two\nAfter") == [
            .init(kind: .item(marker: "•"), text: "one"),
            .init(kind: .continuation, text: "more of one"),
            .init(kind: .item(marker: "•"), text: "two", level: 1),
            .init(kind: .continuation, text: "more of two", level: 1),
            .init(kind: .text, text: "After"),
        ])
        #expect(lines("Intro\n  indented").map(\.kind) == [.text, .text])
    }

    @Test func aParagraphOfOnlyBoldTextIsALabel() {
        #expect(lines("Done.\n\n**You need to:**\n1. Approve") == [
            .init(kind: .text, text: "Done."),
            .init(kind: .label, text: "You need to", gap: true),
            .init(kind: .item(marker: "1."), text: "Approve"),
        ])
        #expect(lines("**In progress**:").first?.kind == .label)
        #expect(lines("- a\n**Next**").last?.kind == .label)
        #expect(lines("## Plan").first == .init(kind: .heading, text: "Plan"))
    }

    @Test func boldThatIsNotAWholeParagraphStaysBold() {
        #expect(lines("**#1921, sign-in:** pushed as a draft").first?.kind == .text)
        #expect(lines("**a** and **b**").first?.kind == .text)
        #expect(lines("Thanks,\n**Tomas**").map(\.kind) == [.text, .text])
        #expect(lines("- **Only bold in an item**").first?.kind == .item(marker: "•"))
        let sentence = "**" + String(repeating: "A long bold warning ", count: 4) + "**"
        #expect(lines(sentence).first?.kind == .text)
        #expect(lines("#251 is merged").first?.kind == .text)
    }

    @Test func labelsHaveMoreSpaceAboveThanBelowAndItemsLessThanParagraphs() {
        let all = lines("Intro\n\n**Steps:**\n1. a\n2. b\n\nOutro")
        let spaces = all.indices.map { MarkdownText.space(before: all[$0], after: $0 > 0 ? all[$0 - 1] : nil) }
        #expect(spaces == [0, MarkdownText.sectionSpace, MarkdownText.underSectionSpace, MarkdownText.itemSpace,
                           MarkdownText.paragraphSpace])
        #expect(MarkdownText.underSectionSpace < MarkdownText.sectionSpace)
        #expect(MarkdownText.itemSpace < MarkdownText.paragraphSpace)
    }
}
