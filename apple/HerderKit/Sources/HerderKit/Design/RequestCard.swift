import Herder
import SwiftUI

/// An approval or a question, answerable in place with large targets.
struct RequestCard: View {
    let request: PendingRequest
    let fleet: Fleet
    /// Shows which session asked, in a row that opens it; off inside that session's own view.
    var showsSession = true
    /// Where the session row opens the session on iPad and the Mac; `nil` pushes it.
    var selection: Binding<SessionKey?>? = nil
    var more = 0
    /// Which of the questions asked together this is, and of how many, when more than one.
    var step: (number: Int, of: Int)? = nil
    /// The session's composer below the card takes the user's own answer, so the card has no
    /// field of its own.
    var answersInComposer = false
    @State private var answer = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            if showsSession { sessionLink }
            // A long question scrolls under the header rather than pushing it out of view.
            ViewThatFits(in: .vertical) {
                content
                ScrollView { content }.frame(maxHeight: 360)
            }
            if let refusal = fleet.refusals[request.session.key] {
                Text(refusal).font(.footnote).foregroundStyle(Theme.failure)
            }
        }
        .padding(14)
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.accent, lineWidth: 1.5))
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: request.isQuestion ? "questionmark.bubble.fill" : "hand.raised.fill")
                .font(.subheadline.weight(.bold))
            Text(request.isQuestion ? "Question" : "Approval needed")
                .font(.subheadline.weight(.semibold))
            if let step {
                StepDots(number: step.number, of: step.of)
            } else if more > 0 {
                Text("+\(more) more").font(.caption.weight(.semibold)).foregroundStyle(Theme.secondary)
            }
            Spacer()
            Text(request.age).font(.caption).foregroundStyle(Theme.tertiary)
        }
        .foregroundStyle(Theme.accent)
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 12) {
            switch request.kind {
            case .approval(let summary):
                Text(summary)
                    .font(Theme.mono)
                    .foregroundStyle(Theme.text)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Theme.raised, in: .rect(cornerRadius: 8))
                    .textSelection(.enabled)
                reasonLines
                HStack(spacing: 10) {
                    ActionButton(title: "Deny", style: .secondary) { await fleet.answer(request, allow: false) }
                    ActionButton(title: "Allow", style: .primary) { await fleet.answer(request, allow: true) }
                }
            case .question(let text, let labels):
                let question = QuestionText(text, choices: labels)
                MarkdownText(text: question.body)
                reasonLines
                VStack(spacing: 6) {
                    ForEach(Array(question.choices.enumerated()), id: \.offset) { index, choice in
                        ChoiceButton(index: index + 1, choice: choice) {
                            await fleet.answer(request, with: .choice(index: UInt32(index)))
                        }
                    }
                    if !answersInComposer { answerField }
                }
                if answersInComposer, !labels.isEmpty {
                    Label("Or type your own answer below", systemImage: "arrow.down")
                        .font(.footnote)
                        .foregroundStyle(Theme.tertiary)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// The user's own answer, sent with Return or the arrow.
    private var answerField: some View {
        HStack(spacing: 8) {
            TextField(labelsEmpty ? "Type an answer…" : "Or type your own answer…", text: $answer)
                .textFieldStyle(.plain)
                .font(.subheadline)
                .submitLabel(.send)
                .onSubmit(sendAnswer)
            Button(action: sendAnswer) {
                Image(systemName: "arrow.up")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(typed.isEmpty ? Theme.tertiary : Theme.onPrimary)
                    .frame(width: 24, height: 24)
                    .background(typed.isEmpty ? Theme.raised : Theme.primary, in: .circle)
            }
            .buttonStyle(.plain)
            .disabled(typed.isEmpty)
            .accessibilityLabel("Send answer")
        }
        .padding(.leading, 12)
        .padding(.trailing, 6)
        .frame(minHeight: ChoiceButton.height)
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }

    private var typed: String { answer.trimmingCharacters(in: .whitespacesAndNewlines) }

    private var labelsEmpty: Bool {
        if case .question(_, let choices) = request.kind { return choices.isEmpty }
        return true
    }

    private func sendAnswer() {
        let text = typed
        guard !text.isEmpty else { return }
        Task { await fleet.answer(request, with: .text(text: text)) }
    }

    /// The session that asks, opening it as a session row does.
    @ViewBuilder private var sessionLink: some View {
        if let selection {
            Button { selection.wrappedValue = request.session.key } label: { sessionLine }
                .buttonStyle(.plain)
        } else {
            NavigationLink(value: NavRoute.session(request.session.key)) { sessionLine }
                .buttonStyle(.plain)
        }
    }

    private var sessionLine: some View {
        HStack(spacing: 6) {
            StatusGlyph(state: request.session.state, size: 7)
            Text(request.session.title).foregroundStyle(Theme.text)
            Text("· \(request.session.machine)").foregroundStyle(Theme.tertiary)
            Spacer(minLength: 8)
            Image(systemName: "chevron.right").font(.caption2.weight(.bold)).foregroundStyle(Theme.tertiary)
        }
        .font(.footnote.weight(.medium))
        .lineLimit(1)
        #if os(iOS)
        .frame(minHeight: 44)
        #else
        .frame(minHeight: 28)
        #endif
        .contentShape(.rect)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("request-session")
        .accessibilityHint("Opens the session")
    }

    @ViewBuilder private var reasonLines: some View {
        if let reason = request.reason {
            Label(reason, systemImage: "arrow.turn.up.right")
                .font(.footnote)
                .foregroundStyle(Theme.secondary)
        }
        if let note = request.note {
            Text("The primary says: \(note)").font(.footnote).italic().foregroundStyle(Theme.secondary)
        }
    }
}

/// "2 of 3" with a dot per question, the answered ones filled.
private struct StepDots: View {
    let number: Int
    let of: Int

    var body: some View {
        HStack(spacing: 6) {
            HStack(spacing: 3) {
                ForEach(1...max(of, 1), id: \.self) { index in
                    Capsule()
                        .fill(index <= number ? Theme.accent : Theme.stroke)
                        .frame(width: index == number ? 12 : 5, height: 5)
                }
            }
            Text("\(number) of \(of)").font(.caption.weight(.semibold).monospacedDigit()).foregroundStyle(Theme.secondary)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Question \(number) of \(of)")
    }
}

/// A full-width button that runs an async action, disabled while it runs.
struct ActionButton: View {
    enum Style { case primary, secondary }
    let title: String
    let style: Style
    let action: () async -> Void
    @State private var running = false

    var body: some View {
        Button {
            running = true
            Task {
                await action()
                running = false
            }
        } label: {
            Group {
                if running { ProgressView().tint(style == .primary ? Theme.onPrimary : Theme.text) } else { Text(title) }
            }
            .font(.body.weight(.semibold))
            .frame(maxWidth: .infinity)
            .frame(height: 48)
            .foregroundStyle(style == .primary ? Theme.onPrimary : Theme.text)
            .background(style == .primary ? Theme.primary : Theme.raised, in: .rect(cornerRadius: Theme.corner))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .disabled(running)
    }
}

/// A question as the card shows it: its text, and its choices with what each means. The Claude
/// adapter writes each choice's description into the text as a `- **label**: description`
/// line; those lines move to their choice, and a label's "(Recommended)" becomes a badge.
struct QuestionText: Equatable {
    struct Choice: Equatable {
        let label: String
        var detail: String?
        var recommended = false
    }

    let body: String
    let choices: [Choice]

    init(_ text: String, choices labels: [String]) {
        var details: [String: String] = [:]
        var lines: [Substring] = []
        for line in text.split(separator: "\n", omittingEmptySubsequences: false) {
            if let match = line.wholeMatch(of: /- \*\*(.+?)\*\*: (.*)/), labels.contains(String(match.output.1)) {
                details[String(match.output.1)] = String(match.output.2)
            } else {
                lines.append(line)
            }
        }
        body = lines.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
        choices = labels.map { label in
            let suffix = " (Recommended)"
            let recommended = label.hasSuffix(suffix)
            return Choice(label: recommended ? String(label.dropLast(suffix.count)) : label,
                          detail: details[label], recommended: recommended)
        }
    }
}

private struct ChoiceButton: View {
    #if os(iOS)
    static let height: CGFloat = 44
    #else
    static let height: CGFloat = 36
    #endif

    let index: Int
    let choice: QuestionText.Choice
    let action: () async -> Void
    @State private var hovering = false

    var body: some View {
        Button {
            Task { await action() }
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text("\(index)")
                    .font(.caption.weight(.bold).monospacedDigit())
                    .foregroundStyle(Theme.secondary)
                    .frame(width: 20, height: 20)
                    .background(Theme.background, in: .rect(cornerRadius: 5))
                    .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 4 }
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(choice.label).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                        if choice.recommended {
                            Text("Recommended")
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(Theme.accent)
                                .padding(.horizontal, 6).padding(.vertical, 1)
                                .background(Theme.accent.opacity(0.15), in: .capsule)
                        }
                    }
                    if let detail = choice.detail {
                        Text(MarkdownText.inline(detail)).font(.footnote).foregroundStyle(Theme.secondary)
                    }
                }
                .multilineTextAlignment(.leading)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .frame(minHeight: Self.height)
            .background(hovering ? Theme.stroke : Theme.raised, in: .rect(cornerRadius: Theme.corner))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
    }
}
