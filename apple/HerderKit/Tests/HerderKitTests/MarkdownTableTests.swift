import Testing
@testable import HerderKit

@MainActor
struct MarkdownTableTests {
    @Test func aHeaderAndSeparatorStartATableWithItsAlignment() throws {
        let text = "Results:\n| Name | Count | State |\n|:-----|------:|:-----:|\n| **api** | 12 | `ok` |\n| web | 3\n\nDone | here"
        let parts = MarkdownText.parse(text)
        #expect(parts.count == 3)
        #expect(parts.first == .line("Results:"))
        guard case .table(let table) = parts[1] else {
            Issue.record("no table in \(parts)")
            return
        }
        #expect(table.header == ["Name", "Count", "State"])
        #expect(table.alignments == [.leading, .trailing, .center])
        #expect(table.rows == [["**api**", "12", "`ok`"], ["web", "3", ""]])
        #expect(parts.last == .line("Done | here"))
    }

    @Test func pipesInProseOrWithoutAValidSeparatorStayText() {
        #expect(MarkdownText.parse("Use a | b to pipe\nthen run it") == [.line("Use a | b to pipe"), .line("then run it")])
        #expect(MarkdownText.parse("| a | b |\n| -- | xx |") == [.line("| a | b |"), .line("| -- | xx |")])
        #expect(MarkdownText.parse("| a | b |\n|---|") == [.line("| a | b |"), .line("|---|")])
        #expect(MarkdownText.parse("| a | b |\n\n|---|---|") == [.line("| a | b |"), .line("|---|---|")])
    }

    @Test func escapedPipesStayInTheirCell() {
        #expect(MarkdownTable.cells(#"| a \| b | c |"#) == ["a | b", "c"])
        #expect(MarkdownTable.cells("x|y") == ["x", "y"])
    }

    @Test func aFenceEndsATable() {
        let parts = MarkdownText.parse("| a |\n| - |\n| 1 |\n```sh\nls\n```")
        #expect(parts.count == 2)
        #expect(parts.last == .code("ls", language: "sh"))
    }
}

#if os(macOS)
import AppKit
import SwiftUI

@MainActor struct MarkdownTableLayoutTests {
    private func size(_ cell: String) -> CGSize {
        var table = MarkdownTable(header: "| A | B |", separator: "|---|---|")!
        table.append("| x | \(cell) |")
        return NSHostingView(rootView: MarkdownTableView(table: table)).fittingSize
    }

    @Test func aLongCellWrapsAndGrowsItsRow() {
        let short = size("short")
        let long = size(String(repeating: "wrapping words ", count: 30))
        #expect(long.width < short.width + 400, "the column is capped, \(long)")
        #expect(long.height > short.height + 60, "the row grows to fit its wrapped lines, \(long) vs \(short)")
    }
}
#endif
