import SwiftUI

/// A GitHub-flavoured pipe table: a header row, a `|---|:-:|` separator, then body rows.
struct MarkdownTable: Equatable {
    enum Alignment: Equatable { case leading, center, trailing }

    var header: [String]
    var alignments: [Alignment]
    var rows: [[String]] = []

    /// A table starts where a pipe row is followed by a separator with as many columns.
    init?(header line: String, separator: String) {
        guard line.contains("|"), separator.contains("|") || separator.contains("-") else { return nil }
        let header = Self.cells(line)
        let cells = Self.cells(separator)
        let alignments = cells.compactMap(Self.alignment)
        guard header.count == cells.count, alignments.count == cells.count else { return nil }
        self.header = header
        self.alignments = alignments
    }

    /// Body rows are padded or cut to the header's width, as GitHub does.
    mutating func append(_ line: String) {
        let cells = Self.cells(line).prefix(header.count)
        rows.append(cells + Array(repeating: "", count: header.count - cells.count))
    }

    /// Cells between unescaped pipes; the outer pipes are optional and `\|` is a literal pipe.
    static func cells(_ line: String) -> [String] {
        var text = Substring(line.trimmingCharacters(in: .whitespaces))
        if text.hasPrefix("|") { text = text.dropFirst() }
        if text.hasSuffix("|") && !text.hasSuffix("\\|") { text = text.dropLast() }
        var cells = [""]
        var escaped = false
        for character in text {
            if escaped {
                cells[cells.count - 1] += character == "|" ? "|" : "\\\(character)"
                escaped = false
            } else if character == "\\" {
                escaped = true
            } else if character == "|" {
                cells.append("")
            } else {
                cells[cells.count - 1].append(character)
            }
        }
        if escaped { cells[cells.count - 1] += "\\" }
        return cells.map { $0.trimmingCharacters(in: .whitespaces) }
    }

    private static func alignment(_ cell: String) -> Alignment? {
        let dashes = cell.trimmingCharacters(in: CharacterSet(charactersIn: ":"))
        guard !dashes.isEmpty, dashes.allSatisfy({ $0 == "-" }) else { return nil }
        switch (cell.hasPrefix(":"), cell.hasSuffix(":")) {
        case (true, true): return .center
        case (false, true): return .trailing
        default: return .leading
        }
    }
}

/// A Markdown table: a raised header and hairlines between rows. Its columns share the width
/// of the message, their long cells wrapping; a table too wide for that even with its columns
/// narrowed scrolls sideways in its own box.
struct MarkdownTableView: View {
    let table: MarkdownTable
    @Environment(\.prLinks) private var prLinks
    @Environment(\.findHighlight) private var find
    /// The width the message gives the table, measured again whenever it changes: the sideways
    /// scroll view proposes its content no width, so the table can't learn it from its proposal.
    @State private var width: CGFloat?

    var body: some View {
        SidewaysScroll {
            TableLayout(columns: table.header.count, width: width) {
                ForEach(Array(table.header.enumerated()), id: \.offset) { column, text in
                    cell(text, column, header: true).background(Theme.raised)
                }
                ForEach(Array(table.rows.enumerated()), id: \.offset) { _, row in
                    ForEach(Array(row.enumerated()), id: \.offset) { column, text in
                        cell(text, column)
                            .overlay(alignment: .top) { Rectangle().fill(Theme.stroke).frame(height: 1) }
                    }
                }
            }
            .clipShape(.rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
    }

    private func cell(_ text: String, _ column: Int, header: Bool = false) -> some View {
        var string = MarkdownText.inline(text, prs: prLinks, find: find)
        if header {
            for run in string.runs {
                string[run.range].inlinePresentationIntent = (run.inlinePresentationIntent ?? []).union(.stronglyEmphasized)
            }
        }
        return TranscriptText(string: string, style: .callout, alignment: textAlignment(table.alignments[column]))
            .padding(.horizontal, 12)
            .padding(.vertical, 7)
            .frame(maxWidth: .infinity, maxHeight: .infinity,
                   alignment: Alignment(horizontal: horizontal(table.alignments[column]), vertical: .center))
    }

    private func horizontal(_ alignment: MarkdownTable.Alignment) -> HorizontalAlignment {
        switch alignment {
        case .leading: .leading
        case .center: .center
        case .trailing: .trailing
        }
    }

    private func textAlignment(_ alignment: MarkdownTable.Alignment) -> NSTextAlignment {
        switch alignment {
        case .leading: .natural
        case .center: .center
        case .trailing: .right
        }
    }
}

/// Lays out a table's cells, given row by row, in columns that fit `width` where they can. Each
/// row is as tall as its tallest cell, so a wrapped cell grows its row.
struct TableLayout: Layout {
    /// How narrow a column gets before the table scrolls instead, padding included.
    static let narrowest: CGFloat = 120

    let columns: Int
    let width: CGFloat?

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let widths = columnWidths(subviews)
        return CGSize(width: widths.reduce(0, +), height: rowHeights(subviews, widths).reduce(0, +))
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let widths = columnWidths(subviews)
        var y = bounds.minY
        for (row, height) in rowHeights(subviews, widths).enumerated() {
            var x = bounds.minX
            for (column, width) in widths.enumerated() {
                subviews[row * columns + column].place(at: CGPoint(x: x, y: y),
                                                       proposal: ProposedViewSize(width: width, height: height))
                x += width
            }
            y += height
        }
    }

    private func columnWidths(_ subviews: Subviews) -> [CGFloat] {
        let natural = (0..<columns).map { column in
            stride(from: column, to: rows(subviews) * columns, by: columns)
                .map { subviews[$0].sizeThatFits(.unspecified).width }.max() ?? 0
        }
        return Self.share(natural, within: width)
    }

    private func rowHeights(_ subviews: Subviews, _ widths: [CGFloat]) -> [CGFloat] {
        (0..<rows(subviews)).map { row in
            widths.enumerated().map { column, width in
                subviews[row * columns + column].sizeThatFits(ProposedViewSize(width: width, height: nil)).height
            }.max() ?? 0
        }
    }

    private func rows(_ subviews: Subviews) -> Int {
        columns == 0 ? 0 : subviews.count / columns
    }

    /// Columns at their natural widths while they fit. Past that each keeps up to `narrowest`, and
    /// the width left over goes to the columns by how much more they would take, so a column of
    /// short words stays whole while long prose wraps. Too narrow for that, the table scrolls.
    static func share(_ natural: [CGFloat], within width: CGFloat?) -> [CGFloat] {
        guard let width, natural.reduce(0, +) > width else { return natural }
        let least = natural.map { min($0, narrowest) }
        let spare = width - least.reduce(0, +)
        guard spare > 0 else { return least }
        let wanted = zip(natural, least).map { $0 - $1 }
        // More than `spare`, as the natural widths overflow `width`.
        let total = wanted.reduce(0, +)
        return zip(least, wanted).map { $0 + spare * $1 / total }
    }
}
