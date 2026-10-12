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
        #expect(MarkdownText.parse("| a | b |\n\n|---|---|") == [.line("| a | b |"), .gap, .line("|---|---|")])
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

    @Test func columnsKeepTheirNaturalWidthsWhileTheyFit() {
        #expect(TableLayout.share([100, 200], within: 400) == [100, 200])
        #expect(TableLayout.share([100, 200], within: nil) == [100, 200])
    }

    @Test func columnsTooWideShareTheWidthByWhatTheyWouldTake() {
        // 120 each, then the 200 left over in proportion to the 0, 380 and 380 more they would take.
        #expect(TableLayout.share([80, 500, 500], within: 520) == [80, 220, 220])
    }

    @Test func columnsThatCantFitKeepTheirNarrowestAndScroll() {
        #expect(TableLayout.share([80, 500, 500], within: 200) == [80, 120, 120])
    }
}

#if os(macOS)
import AppKit
import SwiftUI

@MainActor struct MarkdownTableLayoutTests {
    private static let wide = {
        var table = MarkdownTable(header: "| Role | Who we act for | Whose name the contract is in | Who the guest's contract is with |",
                                  separator: "|---|---|---|---|")!
        table.append("| Commissionaire | Guest | Ours, on the guest's account | Us, while the guest's stay is with the hotel |")
        return table
    }()

    /// The table's laid-out size in a column of each of `widths` in turn, in a window that keeps
    /// its size, as when the sidebar opens or closes.
    private func sizes(_ table: MarkdownTable, at widths: [CGFloat]) throws -> [CGSize] {
        let column = Column()
        let host = NSHostingView(rootView: ColumnView(column: column, table: table))
        let window = NSWindow(contentRect: CGRect(x: 0, y: 0, width: 1400, height: 600), styleMask: .borderless,
                              backing: .buffered, defer: false)
        window.contentView = host
        return try widths.map { width in
            column.width = width
            for _ in 0..<3 {
                host.layoutSubtreeIfNeeded()
                RunLoop.main.run(until: Date().addingTimeInterval(0.05))
            }
            let scroll = try #require(Self.first(NSScrollView.self, in: host))
            return try #require(scroll.documentView).frame.size
        }
    }

    @Test func aWideTableWrapsToTheWidthItIsGiven() throws {
        let sizes = try sizes(Self.wide, at: [1200, 560])
        #expect(sizes[1].width <= 561, "the table fits the narrower column, \(sizes)")
        #expect(sizes[1].height > sizes[0].height, "its cells wrap onto more lines, \(sizes)")
    }

    @Test func aTableFitsAgainWhenItsColumnNarrowsAndWidens() throws {
        let sizes = try sizes(Self.wide, at: [560, 800, 560])
        #expect(sizes.allSatisfy { $0.width <= 801 }, "\(sizes)")
        #expect(sizes[0] == sizes[2], "the same width lays the table out the same, \(sizes)")
        #expect(sizes[1].width > sizes[0].width, "a wider column gives the table more room, \(sizes)")
    }

    @Test func aLongCellWrapsAndGrowsItsRow() throws {
        func size(_ cell: String) throws -> CGSize {
            var table = MarkdownTable(header: "| A | B |", separator: "|---|---|")!
            table.append("| x | \(cell) |")
            return try sizes(table, at: [500])[0]
        }
        let short = try size("short")
        let long = try size(String(repeating: "wrapping words ", count: 30))
        #expect(long.width <= 501, "the column wraps at the width it is given, \(long)")
        #expect(long.height > short.height + 60, "the row grows to fit its wrapped lines, \(long) vs \(short)")
    }

    @MainActor @Observable private final class Column {
        var width: CGFloat = 0
    }

    private struct ColumnView: View {
        let column: Column
        let table: MarkdownTable

        var body: some View {
            MarkdownTableView(table: table).frame(width: column.width).frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private static func first<View: NSView>(_ type: View.Type, in view: NSView) -> View? {
        if let view = view as? View { return view }
        return view.subviews.lazy.compactMap { first(type, in: $0) }.first
    }
}
#endif
