import Foundation

/// What Shift-Enter does in the composer: a new line, continuing a numbered or bulleted list
/// ("1. a" gives "2. "), and ending the list when its last item is empty.
enum ListContinuation {
    static func newline(after text: String) -> String {
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        let last = String(lines.last ?? "")
        guard let marker = marker(of: last) else { return text + "\n" }
        if last.trimmingCharacters(in: .whitespaces) == marker.current.trimmingCharacters(in: .whitespaces) {
            // An empty item ends the list: drop its marker.
            return lines.dropLast().joined(separator: "\n") + (lines.count > 1 ? "\n" : "")
        }
        return text + "\n" + marker.next
    }

    /// The list marker a line starts with, and the one the next line gets.
    private static func marker(of line: String) -> (current: String, next: String)? {
        let indent = String(line.prefix { $0 == " " || $0 == "\t" })
        let rest = line.dropFirst(indent.count)
        for bullet in ["- ", "* ", "• "] where rest.hasPrefix(bullet) {
            return (indent + bullet, indent + bullet)
        }
        let digits = rest.prefix { $0.isNumber }
        if let number = Int(digits), rest.dropFirst(digits.count).hasPrefix(". ") {
            return (indent + "\(number). ", indent + "\(number + 1). ")
        }
        return nil
    }
}
