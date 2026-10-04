import Foundation

/// The composer's text: what the user typed, with a marker for each image and each long paste,
/// `[Image #N]` and `[Pasted text #N]`, numbered in the order they were added. The composer
/// draws the markers as chips; an image's marker goes to the agent as is, so the prompt can
/// refer to it, and a paste's is replaced by the text when the prompt is sent.
enum PromptText {
    enum Kind: Hashable {
        case image, paste

        fileprivate var word: String { self == .image ? "Image" : "Pasted text" }
    }

    /// A marker in the text: the `number`th image or paste.
    struct Token: Hashable {
        let kind: Kind
        let number: Int
        var marker: String { PromptText.marker(kind, number) }
    }

    static func marker(_ kind: Kind, _ number: Int) -> String { "[\(kind.word) #\(number)]" }

    /// The text's markers, in order, with where they are.
    static func tokens(in text: String) -> [(range: Range<String.Index>, token: Token)] {
        text.matches(of: /\[(Image|Pasted text) #(\d+)\]/).compactMap { match in
            guard let number = Int(match.output.2) else { return nil }
            return (match.range, Token(kind: match.output.1 == "Image" ? .image : .paste, number: number))
        }
    }

    /// Whether pasted text is long enough to go in as a chip rather than inline.
    static func isLong(_ pasted: String) -> Bool {
        pasted.count >= 1000 || pasted.reduce(0) { $1 == "\n" ? $0 + 1 : $0 } >= 15
    }

    /// The prompt as sent: each paste's marker replaced by its text.
    static func expand(_ text: String, pastes: [String]) -> String {
        var out = text
        for (index, paste) in pastes.enumerated().reversed() {
            out = out.replacingOccurrences(of: marker(.paste, index + 1), with: paste)
        }
        return out
    }

    /// Drops the items of a kind whose marker is gone from the text, and renumbers the markers
    /// after each, so markers and items stay one to one.
    static func prune<Item>(_ kind: Kind, items: inout [Item], text: inout String) {
        for index in items.indices.reversed() where !text.contains(marker(kind, index + 1)) {
            remove(kind, at: index, items: &items, text: &text)
        }
    }

    /// Removes an item with its marker, renumbering the markers after it.
    static func remove<Item>(_ kind: Kind, at index: Int, items: inout [Item], text: inout String) {
        let count = items.count
        items.remove(at: index)
        text = text.replacingOccurrences(of: marker(kind, index + 1) + " ", with: "")
            .replacingOccurrences(of: marker(kind, index + 1), with: "")
        for number in (index + 2)..<(count + 1) {
            text = text.replacingOccurrences(of: marker(kind, number), with: marker(kind, number - 1))
        }
    }
}
