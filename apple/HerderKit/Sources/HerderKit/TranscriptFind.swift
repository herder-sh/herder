import Foundation
import Herder
import SwiftUI

extension EnvironmentValues {
    /// Where `#123` in the transcript leads.
    @Entry var prLinks = PRLinks()
    /// What the transcript's find marks in the block it reaches, if a find is open.
    @Entry var findHighlight: FindHighlight?
}

/// Where `#123` in a session's text leads: a pull request the session knows by that number,
/// else that number in the repository its pull requests are in.
struct PRLinks: Equatable {
    private var known: [UInt64: URL] = [:]
    private var repository: URL?

    init(_ prs: [PullRequest] = []) {
        for pr in prs {
            if known[pr.number] == nil, let url = URL(string: pr.url) { known[pr.number] = url }
            if repository == nil { repository = Self.repository(of: pr.url) }
        }
    }

    var isEmpty: Bool { known.isEmpty && repository == nil }

    func url(_ number: UInt64) -> URL? {
        known[number] ?? repository?.appending(path: "pull/\(number)")
    }

    /// `https://github.com/o/r` of `https://github.com/o/r/pull/12`.
    static func repository(of url: String) -> URL? {
        guard let range = url.range(of: #"/pull/\d+/?$"#, options: .regularExpression) else { return nil }
        return URL(string: String(url[..<range.lowerBound]))
    }
}

/// What a find marks in a block of text: every occurrence of the query, stronger in the block
/// the find is on.
struct FindHighlight: Equatable {
    let query: String
    let current: Bool

    func mark(_ string: inout AttributedString) {
        guard !query.isEmpty else { return }
        var rest = string.startIndex..<string.endIndex
        while let range = string[rest].range(of: query, options: [.caseInsensitive, .diacriticInsensitive]) {
            string[range].backgroundColor = Theme.accent.opacity(current ? 0.55 : 0.25)
            rest = range.upperBound..<string.endIndex
        }
    }
}

/// Find in a transcript: the query, and which of the blocks it matches is shown.
struct TranscriptFind: Equatable {
    var shown = false
    var query = ""
    /// The block the find is on, by id, so it stays put as the transcript grows.
    var current: String?

    /// The blocks whose text holds the query, in transcript order.
    func matches(_ blocks: [TranscriptBlock]) -> [String] {
        guard !query.isEmpty else { return [] }
        return blocks.filter { $0.searchText.localizedStandardContains(query) }.map(\.id)
    }

    /// Moves to the next match after the current one, or the previous with `by: -1`, wrapping
    /// around; with none current, to the last, the one nearest the end where a session is read.
    mutating func step(by step: Int, in matches: [String]) {
        guard !matches.isEmpty else { current = nil; return }
        guard let current, let index = matches.firstIndex(of: current) else {
            self.current = matches.last
            return
        }
        self.current = matches[(index + step + matches.count) % matches.count]
    }

    func highlight(_ id: String) -> FindHighlight? {
        shown && !query.isEmpty ? FindHighlight(query: query, current: id == current) : nil
    }
}

extension TranscriptBlock {
    /// The text a find looks in.
    var searchText: String {
        switch self {
        case .user(_, let text, _, _, _, _), .assistant(_, let text, _), .reasoning(_, let text, _): text
        case .tools(_, let calls): calls.map { "\($0.name) \($0.summary) \($0.output)" }.joined(separator: "\n")
        case .children(_, let children): children.map(\.task).joined(separator: "\n")
        case .report(let report): report.summary
        case .artifact(let artifact): [artifact.title, artifact.url?.absoluteString ?? ""].joined(separator: "\n")
        case .agents(_, let agents): agents.map { [$0.title, $0.prompt, $0.result ?? ""].joined(separator: "\n") }.joined(separator: "\n")
        case .notice(let notice): [notice.text, notice.detail ?? ""].joined(separator: "\n")
        case .question(let question): ([question.text] + question.choices).joined(separator: "\n")
        case .work(let work): work.blocks.map(\.searchText).joined(separator: "\n")
        case .working, .handoff: ""
        }
    }
}

/// The find bar over a transcript: the query, where the find is among its matches, and the
/// buttons to step through them.
struct FindBar: View {
    @Binding var find: TranscriptFind
    let matches: [String]
    @FocusState private var focused: Bool

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass").foregroundStyle(Theme.tertiary)
            TextField("Find in transcript", text: $find.query)
                .textFieldStyle(.plain)
                .foregroundStyle(Theme.text)
                .focused($focused)
                .frame(minWidth: 140, maxWidth: 220)
                .accessibilityIdentifier("find-field")
                // Return walks up to older matches, as the find starts at the newest; with
                // Shift, back down.
                .onKeyPress(keys: [.return]) { press in
                    find.step(by: press.modifiers.contains(.shift) ? 1 : -1, in: matches)
                    return .handled
                }
            if !find.query.isEmpty {
                Text(position).font(.caption.monospacedDigit()).foregroundStyle(Theme.tertiary)
            }
            Button("Previous Match", systemImage: "chevron.up") { find.step(by: -1, in: matches) }
                .keyboardShortcut("g", modifiers: [.command, .shift])
                .disabled(matches.isEmpty)
            Button("Next Match", systemImage: "chevron.down") { find.step(by: 1, in: matches) }
                .keyboardShortcut("g", modifiers: .command)
                .disabled(matches.isEmpty)
            Button("Done") { find.shown = false }
                .keyboardShortcut(.cancelAction)
                .font(.callout.weight(.semibold))
        }
        .labelStyle(.iconOnly)
        .buttonStyle(.plain)
        .foregroundStyle(Theme.secondary)
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Theme.raised, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
        .onAppear {
            focused = true
            land()
        }
        .onChange(of: matches) { land() }
    }

    /// Keeps the find on its match while the query still matches it, else on the newest.
    private func land() {
        if find.current.map(matches.contains) != true { find.step(by: -1, in: matches) }
    }

    private var position: String {
        guard let current = find.current, let index = matches.firstIndex(of: current) else { return "No matches" }
        return "\(index + 1) of \(matches.count)"
    }
}
