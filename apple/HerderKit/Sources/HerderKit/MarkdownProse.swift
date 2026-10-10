import SwiftUI

/// The typography of a run of prose lines: list items with a hanging indent, section labels,
/// and the space between them. Every rule reads as plain prose when it misfires.
extension MarkdownText {
    /// A line of prose as Markdown reads it, its marks dropped.
    struct ProseLine: Equatable {
        enum Kind: Equatable {
            case text, heading, quote
            /// A paragraph of only bold text, as agents title a section: "**You need to:**".
            case label
            /// A list item, with its number as written ("1.", "3)") or a bullet.
            case item(marker: String)
            /// An indented line under a list item, which goes on with it.
            case continuation
        }

        var kind: Kind
        var text: String
        /// How deep in a list: 0 for a top-level item and the lines that go on with it.
        var level = 0
        /// Whether a blank line comes before it.
        var gap = false

        fileprivate var inList: Bool {
            switch kind {
            case .item, .continuation: true
            default: false
            }
        }

        var section: Bool { kind == .heading || kind == .label }
    }

    /// A bold label longer than this is a bold sentence, which stays one.
    static let labelLimit = 60

    nonisolated static func proseLines(_ block: [Part]) -> [ProseLine] {
        var lines: [ProseLine] = []
        // The indent of the items of each open list, outermost first.
        var indents: [Int] = []
        var gap = false
        for part in block {
            guard case .line(let raw) = part else {
                gap = true
                continue
            }
            let indent = raw.prefix { $0 == " " || $0 == "\t" }.reduce(0) { $0 + ($1 == "\t" ? 4 : 1) }
            let line = raw.drop { $0 == " " || $0 == "\t" }
            if let item = listItem(line) {
                while let last = indents.last, last > indent { indents.removeLast() }
                if let last = indents.last, indent < last + 2 {
                    indents[indents.count - 1] = indent
                } else {
                    indents.append(indent)
                }
                lines.append(ProseLine(kind: .item(marker: item.marker), text: item.text, level: indents.count - 1, gap: gap))
            } else if indent >= 2, let previous = lines.last, previous.inList {
                lines.append(ProseLine(kind: .continuation, text: String(line), level: previous.level, gap: gap))
            } else {
                indents = []
                // Only a line that starts a paragraph is a label: "Thanks,\n**Tomas**" stays bold.
                let starts = gap || lines.last.map { $0.inList || $0.section } ?? true
                lines.append(paragraphLine(line, starts: starts, gap: gap))
            }
            gap = false
        }
        return lines
    }

    private nonisolated static func listItem(_ line: Substring) -> (marker: String, text: String)? {
        if let match = line.wholeMatch(of: /[-*+][ \t]+(.*)/) { return ("•", String(match.1)) }
        if let match = line.wholeMatch(of: /(\d{1,9}[.)])[ \t]+(.*)/) { return (String(match.1), String(match.2)) }
        return nil
    }

    private nonisolated static func paragraphLine(_ line: Substring, starts: Bool, gap: Bool) -> ProseLine {
        if let match = line.wholeMatch(of: /#{1,6}(?:[ \t]+(.*))?/) {
            return ProseLine(kind: .heading, text: (match.1.map(String.init) ?? "").trimmingCharacters(in: .whitespaces), gap: gap)
        }
        if line.hasPrefix(">") {
            return ProseLine(kind: .quote, text: line.dropFirst().trimmingCharacters(in: .whitespaces), gap: gap)
        }
        if starts, let match = line.wholeMatch(of: /\*\*((?:(?!\*\*).)+)\*\*:?[ \t]*/) {
            var label = match.1.trimmingCharacters(in: .whitespaces)
            if label.hasSuffix(":") { label.removeLast() }
            if !label.isEmpty && label.count <= labelLimit { return ProseLine(kind: .label, text: label, gap: gap) }
        }
        return ProseLine(kind: .text, text: String(line), gap: gap)
    }

    /// What a prose run is set in, which the text views turn into their fonts.
    enum Role: AttributedStringKey {
        typealias Value = RoleValue
        static let name = "herder.prose.role"
    }

    enum RoleValue: Hashable, Sendable {
        case heading, label, quote
        /// A list item's bullet or number, its digits all one width.
        case marker
    }

    /// Where a prose paragraph sits: how many indent columns, whether its first line starts a
    /// column to the left with a list marker, and the space above it.
    struct Paragraph: AttributedStringKey, Hashable, Sendable {
        typealias Value = Paragraph
        static let name = "herder.prose.paragraph"

        var indent = 0
        var marker = false
        var spaceBefore: CGFloat = 0
    }

    static let paragraphSpace: CGFloat = 10
    static let itemSpace: CGFloat = 4
    static let sectionSpace: CGFloat = 20
    static let underSectionSpace: CGFloat = 6

    /// The space above `line`, after `previous`.
    nonisolated static func space(before line: ProseLine, after previous: ProseLine?) -> CGFloat {
        guard let previous else { return 0 }
        if line.section { return sectionSpace }
        if previous.section { return underSectionSpace }
        if line.gap { return paragraphSpace }
        if case .item = line.kind { return itemSpace }
        return 0
    }

    /// A run of prose lines as one text, each line a paragraph of its own.
    func prose(_ lines: [ProseLine], last: Bool) -> AttributedString {
        var string = AttributedString()
        for (index, line) in lines.enumerated() {
            var piece = styled(line)
            if streaming && last && index == lines.count - 1 { piece += AttributedString(" ▍") }
            if index < lines.count - 1 { piece += AttributedString("\n") }
            let indented = line.inList ? line.level + 1 : 0
            piece[Paragraph.self] = Paragraph(indent: indented, marker: line.kind != .continuation && line.inList,
                                              spaceBefore: Self.space(before: line, after: index > 0 ? lines[index - 1] : nil))
            string += piece
        }
        return string
    }

    private func styled(_ line: ProseLine) -> AttributedString {
        switch line.kind {
        case .text, .continuation:
            return inline(line.text)
        case .heading:
            var heading = inline(line.text)
            heading[Role.self] = .heading
            return heading
        case .label:
            let source = inline(line.text)
            // Upper case reads as a label; code and links keep their case.
            var label = AttributedString()
            for run in source.runs {
                let kept = run.link != nil || run.inlinePresentationIntent?.contains(.code) == true
                label += kept ? AttributedString(source[run.range])
                    : AttributedString(String(source[run.range].characters).uppercased(), attributes: run.attributes)
            }
            label[Role.self] = .label
            label.foregroundColor = Theme.secondary
            return label
        case .quote:
            var quote = inline(line.text)
            quote[Role.self] = .quote
            quote.foregroundColor = Theme.secondary
            return quote
        case .item(let marker):
            var mark = AttributedString(marker + "\t")
            mark[Role.self] = .marker
            mark.foregroundColor = Theme.tertiary
            return mark + inline(line.text)
        }
    }

    /// `text`'s inline Markdown, its code spans set on a raised ground.
    private func inline(_ text: String) -> AttributedString {
        var string = Self.markdown(text)
        for run in string.runs where run.inlinePresentationIntent?.contains(.code) == true {
            string[run.range].backgroundColor = Theme.raised
        }
        return Self.decorated(string, prs: prLinks, find: find)
    }
}
