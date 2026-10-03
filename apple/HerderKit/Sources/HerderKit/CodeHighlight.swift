import Foundation
import SwiftUI

/// A small lexical highlighter. Unknown languages remain plain, and no tokenization changes
/// the original text. Ordered alternatives keep comment markers inside strings as strings.
enum CodeHighlight {
    enum Kind { case string, comment, number, keyword, key }
    struct Token {
        let range: NSRange
        let kind: Kind
    }

    static func tokens(_ code: String, language: String) -> [Token] {
        let language = language.lowercased()
        let hashComments = ["toml", "yaml", "yml", "python", "py", "bash", "sh", "shell", "ruby", "rb"]
        let slashComments = ["swift", "rust", "rs", "javascript", "js", "typescript", "ts", "tsx", "jsx", "c", "cpp", "c++", "java", "go", "jsonc"]
        guard hashComments.contains(language) || slashComments.contains(language) || language == "json" else { return [] }
        let strings = #"(?:\"\"\"[\s\S]*?(?:\"\"\"|$)|'''[\s\S]*?(?:'''|$)|\"(?:\\.|[^\"\\])*(?:\"|$)|'(?:\\.|[^'\\])*(?:'|$)|`(?:\\.|[^`\\])*(?:`|$))"#
        let comments = hashComments.contains(language) ? #"#[^\n]*"#
            : slashComments.contains(language) ? #"//[^\n]*|/\*[\s\S]*?(?:\*/|$)"# : #"(?!)"#
        let number = #"\b(?:0[xX][0-9a-fA-F_]+|\d[\d_]*(?:\.\d+)?(?:[eE][+-]?\d+)?)\b"#
        let keyword = #"\b(?:true|false|null|nil|None|True|False|let|var|const|func|fn|def|class|struct|enum|impl|trait|interface|type|import|from|use|pub|public|private|return|if|else|elif|for|while|in|match|switch|case|break|continue|async|await|try|catch|throw|throws|new|export|default|mut|self|this|guard|do|end|then|fi)\b"#
        let key = language == "toml" ? #"(?m)^\s*\[\[?[^\]\n]+\]\]?|\b[A-Za-z_][\w.-]*(?=\s*=)"#
            : ["yaml", "yml"].contains(language) ? #"(?m)^\s*[\w.-]+(?=\s*:)"# : #"(?!)"#
        let pattern = [strings, comments, number, keyword, key].map { "(" + $0 + ")" }.joined(separator: "|")
        guard let regex = try? NSRegularExpression(pattern: pattern) else { return [] }
        let kinds: [Kind] = [.string, .comment, .number, .keyword, .key]
        return regex.matches(in: code, range: NSRange(code.startIndex..., in: code)).compactMap { match in
            for (index, kind) in kinds.enumerated() where match.range(at: index + 1).location != NSNotFound {
                return Token(range: match.range, kind: kind)
            }
            return nil
        }
    }

    static func attributed(_ code: String, language: String) -> AttributedString {
        var result = AttributedString(code)
        for token in tokens(code, language: language) {
            guard let range = Range(token.range, in: code),
                  let start = AttributedString.Index(range.lowerBound, within: result),
                  let end = AttributedString.Index(range.upperBound, within: result) else { continue }
            let color: Color
            switch token.kind {
            case .string: color = Color(light: 0x326529, dark: 0xA3D192)
            case .comment: color = Color(light: 0x626970, dark: 0x8C96A1)
            case .number: color = Color(light: 0x8A4C12, dark: 0xF0B06E)
            case .keyword: color = Color(light: 0x754499, dark: 0xC7A3E8)
            case .key: color = Color(light: 0x236A89, dark: 0x7AC2E6)
            }
            result[start..<end].foregroundColor = color
        }
        return result
    }
}
