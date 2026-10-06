import Foundation
import Testing
@testable import HerderKit

@MainActor
struct TranscriptFindTests {
    private let blocks: [TranscriptBlock] = [
        .user(id: "u1", text: "Fix the race trails", outgoing: nil),
        .assistant(id: "a1", text: "Replaced the trails.", streaming: false),
        .tools(id: "t1", calls: [ToolCall(id: "c1", name: "Bash", kind: .command, summary: "cargo test", outcome: .ok)]),
        .assistant(id: "a2", text: "Tests pass; the TRAILS are gone.", streaming: false),
    ]

    @Test func findMatchesBlocksCaseInsensitivelyAndStartsAtTheNewest() {
        var find = TranscriptFind(shown: true, query: "trails")
        let matches = find.matches(blocks)
        #expect(matches == ["u1", "a1", "a2"])
        #expect(find.matches(blocks).contains("t1") == false)
        find.step(by: -1, in: matches)
        #expect(find.current == "a2")
        find.step(by: -1, in: matches)
        #expect(find.current == "a1")
        find.step(by: 1, in: matches)
        find.step(by: 1, in: matches)
        #expect(find.current == "u1")
        #expect(TranscriptFind(shown: true, query: "cargo").matches(blocks) == ["t1"])
    }

    @Test func theCurrentBlockIsMarkedStrongerThanTheRest() {
        let find = TranscriptFind(shown: true, query: "trails", current: "a2")
        #expect(find.highlight("a2")?.current == true)
        #expect(find.highlight("a1")?.current == false)
        #expect(TranscriptFind(shown: false, query: "trails").highlight("a1") == nil)

        var string = AttributedString("Trails and trails")
        FindHighlight(query: "trails", current: true).mark(&string)
        #expect(string.runs.filter { $0.backgroundColor != nil }.map { String(string[$0.range].characters) }
            == ["Trails", "trails"])
    }
}
