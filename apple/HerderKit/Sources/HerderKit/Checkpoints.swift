import Observation
import SwiftUI

/// The user's messages in a transcript, as places to jump back to.
struct Checkpoints: Equatable {
    struct Checkpoint: Equatable, Identifiable {
        /// The id of the user message's block.
        let id: String
        let prompt: String
        /// The first prose the agent answered with, if it has yet.
        let reply: String?
    }

    private(set) var items: [Checkpoint] = []
    /// The checkpoint each block falls under; blocks before the first message fall under none.
    private var owners: [String: Int] = [:]

    init(_ blocks: [TranscriptBlock]) {
        for block in blocks {
            switch block {
            case .user(let id, let text, _, _, _):
                items.append(Checkpoint(id: id, prompt: Self.flat(text), reply: nil))
            case .assistant(_, let text, _):
                if let last = items.indices.last, items[last].reply == nil, !text.isEmpty {
                    items[last] = Checkpoint(id: items[last].id, prompt: items[last].prompt, reply: Self.flat(text))
                }
            default:
                break
            }
            if let last = items.indices.last { owners[block.id] = last }
        }
    }

    /// The checkpoint the block at the top of the view falls under.
    func current(top: String?) -> Int? { top.flatMap { owners[$0] } }

    private static func flat(_ text: String) -> String {
        text.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }.joined(separator: " ")
    }
}

/// The block at the top of the transcript, observed apart from the session view so only the
/// rail redraws as it scrolls.
@Observable
final class TranscriptTop {
    var id: String?
}

/// A tick per user message down the transcript's leading edge, the one in view marked: hover
/// one for the message and the start of its reply, click to scroll to it.
struct CheckpointRail: View {
    let checkpoints: Checkpoints
    let top: TranscriptTop
    let jump: (String) -> Void
    @State private var hovered: Int?

    private static let step: CGFloat = 10
    private static let width: CGFloat = 22

    var body: some View {
        GeometryReader { geometry in
            let items = checkpoints.items
            // Ticks spread 10 points apart, closer when there are more than fit.
            let step = min(Self.step, max(geometry.size.height - 40, 0) / CGFloat(max(items.count, 1)))
            let current = checkpoints.current(top: top.id)
            let index = { (y: CGFloat) in min(max(Int(y / step), 0), items.count - 1) }
            VStack(spacing: 0) {
                ForEach(Array(items.enumerated()), id: \.element.id) { offset, _ in
                    let marked = offset == current || offset == hovered
                    Capsule()
                        .fill(marked ? Theme.text : Theme.tertiary.opacity(0.6))
                        .frame(width: marked ? 14 : 8, height: min(2, step / 2))
                        .frame(width: Self.width, height: step, alignment: .leading)
                }
            }
            .padding(.leading, 6)
            .contentShape(.rect)
            .onContinuousHover { phase in
                switch phase {
                case .active(let location): hovered = index(location.y)
                case .ended: hovered = nil
                }
            }
            .onTapGesture { location in jump(items[index(location.y)].id) }
            .overlay(alignment: .topLeading) {
                if let hovered, items.indices.contains(hovered) {
                    CheckpointCard(checkpoint: items[hovered])
                        .offset(x: Self.width + 12, y: max(CGFloat(hovered) * step - 20, 0))
                        .allowsHitTesting(false)
                }
            }
            .frame(maxHeight: .infinity)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Your messages")
            .accessibilityIdentifier("checkpoints")
        }
        .frame(width: Self.width + 6)
    }
}

/// A user message and the start of its reply, as the rail shows it on hover.
private struct CheckpointCard: View {
    let checkpoint: Checkpoints.Checkpoint

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(checkpoint.prompt).font(.callout.weight(.semibold)).foregroundStyle(Theme.text).lineLimit(1)
            if let reply = checkpoint.reply {
                Text(MarkdownText.inline(reply)).font(.callout).foregroundStyle(Theme.tertiary).lineLimit(3)
            }
        }
        .padding(14)
        .frame(width: 340, alignment: .leading)
        .background(Theme.surface, in: .rect(cornerRadius: 14))
        .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(Theme.stroke))
        .shadow(color: .black.opacity(0.25), radius: 12, y: 4)
    }
}
