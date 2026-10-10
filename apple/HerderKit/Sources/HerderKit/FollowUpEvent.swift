import Herder
import SwiftUI

extension FollowUp {
    /// What happened, from the reason and the pull request: "CI failed on #345".
    var headline: String {
        switch reason {
        case .ciFailed: pr.map { "CI failed on #\($0)" } ?? "CI failed"
        case .ciPassed: pr.map { "CI passed on #\($0)" } ?? "CI passed"
        case .conflicting: pr.map { "#\($0) has merge conflicts" } ?? "The pull request has merge conflicts"
        case .changesRequested: pr.map { "Changes requested on #\($0)" } ?? "Changes requested"
        case .stalled: "Agent stopped with work unfinished"
        case .unknown: pr.map { "herder followed up on #\($0)" } ?? "herder followed up"
        }
    }

    /// What herder asked the agent to do about it.
    var request: String {
        switch reason {
        case .ciFailed: "herder asked the agent to fix it and push"
        case .ciPassed: "herder asked the agent to finish it"
        case .conflicting: "herder asked the agent to rebase"
        case .changesRequested: "herder asked the agent to address the review"
        case .stalled: "herder asked the agent to carry on"
        case .unknown: "herder sent the agent a prompt"
        }
    }

    /// The Board column a pull request in this state lands in, whose symbol and colour it shares.
    private var state: WorkState? {
        switch reason {
        case .ciFailed: .ciFailed
        case .ciPassed: .readyToMerge
        case .conflicting: .conflicting
        case .changesRequested: .changesRequested
        case .stalled, .unknown: nil
        }
    }

    var symbol: String {
        state?.symbol ?? (reason == .stalled ? "pause.circle.fill" : "arrow.triangle.pull")
    }

    var tint: Color {
        state?.tint ?? (reason == .stalled ? Theme.accent : Theme.secondary)
    }
}

/// A prompt herder sent the agent on its own, as an event on the agent's side: what happened,
/// and under it what herder asked, which a click opens to the prompt itself.
struct FollowUpEvent: View {
    let id: String
    let followUp: FollowUp
    let text: String
    @Environment(\.prLinks) private var prLinks
    @Environment(\.findHighlight) private var find
    @Environment(\.transcriptExpanded) private var expanded

    var body: some View {
        let open = expanded.wrappedValue.contains(id)
        VStack(alignment: .leading, spacing: 6) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 8) {
                    Image(systemName: followUp.symbol)
                        .foregroundStyle(followUp.tint)
                        .frame(width: 18)
                    Text(MarkdownText.decorated(AttributedString(followUp.headline), prs: prLinks, find: find))
                        .fontWeight(.semibold)
                        .foregroundStyle(followUp.tint)
                        .tint(Theme.link)
                        .layoutPriority(1)
                    Rectangle().fill(Theme.stroke).frame(height: 1)
                }
                .font(.callout)
                Button {
                    if open { expanded.wrappedValue.remove(id) } else { expanded.wrappedValue.insert(id) }
                } label: {
                    HStack(spacing: 4) {
                        Text(followUp.request)
                        Image(systemName: "chevron.right")
                            .font(.caption2.weight(.semibold))
                            .rotationEffect(.degrees(open ? 90 : 0))
                    }
                    .font(.caption)
                    .foregroundStyle(Theme.tertiary)
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .help(open ? "Hide the prompt" : "Show the prompt")
                .padding(.leading, 26)
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("\(followUp.headline). \(followUp.request)")
            .accessibilityValue(open ? "Expanded" : "Collapsed")
            if open {
                TranscriptText(string: MarkdownText.decorated(AttributedString(text), prs: prLinks, find: find),
                               style: .footnote, color: Theme.secondary)
                    .tint(Theme.link)
                    .padding(.leading, 10)
                    .overlay(alignment: .leading) { Rectangle().fill(Theme.stroke).frame(width: 2) }
                    .padding(.leading, 26)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
