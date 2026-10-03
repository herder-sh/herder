import Foundation
import SwiftUI
@testable import HerderKit
import Testing

struct CodeHighlightTests {
    @Test func tomlColorsSectionsKeysStringsNumbersAndComments() {
        let code = "[[accounts]]\nid = \"claude-main\" # work\nlimit = 42\nenabled = true"
        let tokens = CodeHighlight.tokens(code, language: "toml")
        #expect(tokens.filter { $0.kind == .key }.count == 4)
        #expect(tokens.filter { $0.kind == .string }.count == 1)
        #expect(tokens.filter { $0.kind == .comment }.count == 1)
        #expect(tokens.filter { $0.kind == .number }.count == 1)
        #expect(tokens.filter { $0.kind == .keyword }.count == 1)
        let highlighted = CodeHighlight.attributed(code, language: "toml")
        #expect(String(highlighted.characters) == code)
        #expect(highlighted.runs.contains { $0.foregroundColor != nil })
    }

    @Test func stringsKeepTheirCommentMarkersAndUnicode() {
        let code = "let url = \"https://example.com/🦀\" // note"
        let tokens = CodeHighlight.tokens(code, language: "swift")
        let comments = tokens.filter { $0.kind == .comment }
        #expect(comments.count == 1)
        #expect((code as NSString).substring(with: comments[0].range) == "// note")
        #expect(String(CodeHighlight.attributed(code, language: "swift").characters) == code)
        #expect(CodeHighlight.tokens(code, language: "unknown").isEmpty)
    }

    @Test func unfinishedFencesKeepLanguageAndExactCode() {
        #expect(MarkdownText.parse("```toml\nid = \"work\"") == [.code("id = \"work\"", language: "toml")])
        #expect(MarkdownText.parse("```python\nprint(42)\n```\nDone") == [
            .code("print(42)", language: "python"), .line("Done")])
        #expect(CodeHighlight.tokens("print(42) # note", language: "python").count == 2)
    }

    @Test(arguments: ["swift", "rust", "javascript", "typescript", "go", "java", "c", "cpp"])
    func commonLanguagesColorKeywordsStringsNumbersAndComments(language: String) {
        let code = "return \"hello\" + 42; // note"
        let kinds = CodeHighlight.tokens(code, language: language).map(\.kind)
        #expect(kinds == [.keyword, .string, .number, .comment])
        #expect(String(CodeHighlight.attributed(code, language: language).characters) == code)
    }

    @Test func jsonDoesNotTreatCommentMarkersAsComments() {
        let code = "{\"url\": \"https://example.com\", \"ok\": true, \"count\": 42}"
        let tokens = CodeHighlight.tokens(code, language: "JSON")
        #expect(!tokens.contains { $0.kind == .comment })
        #expect(tokens.filter { $0.kind == .string }.count == 4)
        #expect(tokens.filter { $0.kind == .keyword }.count == 1)
        #expect(tokens.filter { $0.kind == .number }.count == 1)
    }

    @Test func unknownAndUnlabelledCodeRetainsWhitespaceWithoutColors() {
        let code = "  🦀\t42\n\n  return true\n"
        for language in ["", "unknown"] {
            let attributed = CodeHighlight.attributed(code, language: language)
            #expect(String(attributed.characters) == code)
            #expect(attributed.runs.allSatisfy { $0.foregroundColor == nil })
        }
        #expect(MarkdownText.parse("```\n  🦀\t42\n\n  return true\n\n```") == [
            .code(code, language: "")])
    }

    @Test func partialStringsAreColoredDuringStreaming() {
        let code = "let name = \"unfinished 🦀"
        let tokens = CodeHighlight.tokens(code, language: "swift")
        #expect(tokens.map(\.kind) == [.keyword, .string])
        #expect(String(CodeHighlight.attributed(code, language: "swift").characters) == code)
        #expect(MarkdownText.parse("```swift\n" + code, streaming: true) == [
            .code(code, language: "swift")])
    }
}
