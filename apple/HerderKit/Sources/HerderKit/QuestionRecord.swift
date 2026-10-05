import Herder
import SwiftUI

/// A question the agent asked, as the transcript keeps it: the question with what each choice
/// means, and once answered, the choice picked or the words typed. While it waits, the card
/// above the composer is where it is answered, so this one only says so.
struct QuestionRecord: View {
    let question: AskedQuestion

    var body: some View {
        let text = QuestionText(question.text, choices: question.choices)
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "questionmark.bubble").foregroundStyle(Theme.secondary)
                Text(question.routedTo == .primary ? "Question for the primary session" : "Question")
                    .fontWeight(.semibold).foregroundStyle(Theme.text)
                Spacer(minLength: 8)
                Text(status).foregroundStyle(waitsForYou ? Theme.accent : Theme.tertiary)
            }
            .font(.footnote)
            if let reason = question.reason {
                Label(reason.text, systemImage: "arrow.turn.up.right").font(.caption).foregroundStyle(Theme.tertiary)
            }
            MarkdownText(text: text.body)
            if question.answer != nil && !text.choices.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(Array(text.choices.enumerated()), id: \.offset) { index, choice in
                        ChoiceLine(choice: choice, picked: index == question.picked)
                    }
                }
            }
            if case .text(let typed)? = question.answer {
                Text(typed)
                    .font(.subheadline)
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Theme.raised, in: .rect(cornerRadius: 8))
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner)
            .strokeBorder(waitsForYou ? Theme.accent.opacity(0.6) : Theme.stroke))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("question-\(question.id)")
    }

    private var waitsForYou: Bool { question.answer == nil && question.routedTo == .user }

    private var status: String {
        switch question.answeredBy {
        case .user?: "Answered"
        case .primary?: "Answered by the primary"
        case nil: question.routedTo == .user ? "Waiting for you" : "Waiting for the primary"
        }
    }
}

/// One choice of an answered question: the one picked stands out, the others step back.
private struct ChoiceLine: View {
    let choice: QuestionText.Choice
    let picked: Bool

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: picked ? "checkmark.circle.fill" : "circle")
                .imageScale(.small)
                .foregroundStyle(picked ? Theme.accent : Theme.tertiary)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(choice.label)
                        .font(.subheadline.weight(picked ? .semibold : .regular))
                        .foregroundStyle(picked ? Theme.text : Theme.secondary)
                    if choice.recommended {
                        Text("Recommended").font(.caption2.weight(.medium)).foregroundStyle(Theme.tertiary)
                    }
                }
                if let detail = choice.detail {
                    Text(MarkdownText.inline(detail)).font(.footnote).foregroundStyle(Theme.tertiary)
                }
            }
        }
        .opacity(picked ? 1 : 0.75)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(picked ? .isSelected : [])
    }
}
