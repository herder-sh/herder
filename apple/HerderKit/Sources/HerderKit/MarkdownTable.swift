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

/// A Markdown table: a raised header, hairlines between rows, and sideways scrolling when it's
/// wider than the transcript. Long cells wrap rather than stretch a column.
struct MarkdownTableView: View {
    let table: MarkdownTable

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                GridRow {
                    ForEach(Array(table.header.enumerated()), id: \.offset) { column, text in
                        cell(text, column).font(.callout.weight(.semibold)).foregroundStyle(Theme.text)
                            .gridColumnAlignment(horizontal(table.alignments[column]))
                    }
                }
                .background(Theme.raised)
                ForEach(Array(table.rows.enumerated()), id: \.offset) { _, row in
                    Rectangle().fill(Theme.stroke).frame(height: 1).gridCellUnsizedAxes(.horizontal)
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { column, text in
                            cell(text, column).font(.callout).foregroundStyle(Theme.text)
                        }
                    }
                }
            }
            .clipShape(.rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
            .textSelection(.enabled)
        }
    }

    private func cell(_ text: String, _ column: Int) -> some View {
        WrappingWidth(limit: 320) {
            Text(MarkdownText.inline(text))
                .multilineTextAlignment(textAlignment(table.alignments[column]))
        }
        .frame(maxWidth: .infinity, alignment: Alignment(horizontal: horizontal(table.alignments[column]), vertical: .center))
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
    }

    private func horizontal(_ alignment: MarkdownTable.Alignment) -> HorizontalAlignment {
        switch alignment {
        case .leading: .leading
        case .center: .center
        case .trailing: .trailing
        }
    }

    private func textAlignment(_ alignment: MarkdownTable.Alignment) -> TextAlignment {
        switch alignment {
        case .leading: .leading
        case .center: .center
        case .trailing: .trailing
        }
    }
}

/// Lays its content out at its natural width up to `limit`, wrapping past it. The sideways
/// scroll view proposes no width, so text would otherwise measure itself on one line and spill
/// out of a row sized for that one line.
private struct WrappingWidth: Layout {
    let limit: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard let content = subviews.first else { return .zero }
        return content.sizeThatFits(ProposedViewSize(width: width(of: content), height: nil))
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        subviews.first?.place(at: bounds.origin, proposal: ProposedViewSize(width: bounds.width, height: nil))
    }

    private func width(of content: LayoutSubview) -> CGFloat {
        min(content.sizeThatFits(.unspecified).width, limit)
    }
}
