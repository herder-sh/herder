import Herder
import SwiftUI

/// An approval or a question, answerable in place with large targets.
struct RequestCard: View {
    let request: PendingRequest
    let fleet: Fleet
    /// Shows which session asked; off inside that session's own view.
    var showsSession = true
    var more = 0
    @State private var answer = ""
    @State private var sending = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: request.isQuestion ? "questionmark.bubble.fill" : "hand.raised.fill")
                    .font(.subheadline.weight(.bold))
                Text(request.isQuestion ? "Question" : "Approval needed")
                    .font(.subheadline.weight(.semibold))
                if more > 0 {
                    Text("+\(more) more").font(.caption.weight(.semibold)).foregroundStyle(Theme.secondary)
                }
                Spacer()
                Text(request.age).font(.caption).foregroundStyle(Theme.tertiary)
            }
            .foregroundStyle(Theme.accent)
            if showsSession {
                HStack(spacing: 6) {
                    StatusGlyph(state: request.session.state, size: 7)
                    Text(request.session.title).foregroundStyle(Theme.text)
                    Text("· \(request.session.machine)").foregroundStyle(Theme.tertiary)
                }
                .font(.footnote.weight(.medium))
                .lineLimit(1)
            }
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
            case .question(let text, let choices):
                Text(text).font(.body).foregroundStyle(Theme.text)
                reasonLines
                VStack(spacing: 8) {
                    ForEach(Array(choices.enumerated()), id: \.offset) { index, choice in
                        ChoiceButton(index: index + 1, title: choice) {
                            await fleet.answer(request, with: .choice(index: UInt32(index)))
                        }
                    }
                    TextField("Or type an answer…", text: $answer)
                        .font(.subheadline)
                        .padding(.horizontal, 12)
                        .frame(minHeight: 44)
                        .background(Theme.raised, in: .rect(cornerRadius: Theme.corner))
                        .submitLabel(.send)
                        .onSubmit {
                            let text = answer.trimmingCharacters(in: .whitespacesAndNewlines)
                            guard !text.isEmpty else { return }
                            Task { await fleet.answer(request, with: .text(text: text)) }
                        }
                }
            }
            if let refusal = fleet.refusals[request.session.key] {
                Text(refusal).font(.footnote).foregroundStyle(Theme.failure)
            }
        }
        .padding(14)
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.accent, lineWidth: 1.5))
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

private struct ChoiceButton: View {
    let index: Int
    let title: String
    let action: () async -> Void

    var body: some View {
        Button {
            Task { await action() }
        } label: {
            HStack(spacing: 10) {
                Text("\(index)")
                    .font(.caption.weight(.bold).monospaced())
                    .foregroundStyle(Theme.secondary)
                    .frame(width: 22, height: 22)
                    .background(Theme.background, in: .rect(cornerRadius: 6))
                Text(title).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                    .multilineTextAlignment(.leading)
                Spacer()
            }
            .padding(.horizontal, 12)
            .frame(minHeight: 48)
            .background(Theme.raised, in: .rect(cornerRadius: Theme.corner))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }
}
