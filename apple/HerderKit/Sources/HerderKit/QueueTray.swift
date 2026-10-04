import Herder
import SwiftUI

/// The messages waiting in a session's queue on its machine, stacked above the composer in the
/// order they will run. Each can be sent now, ahead of the rest, removed, or dragged to another
/// place, and a user's messages can be merged into one; edits from other clients arrive with the
/// session list.
struct QueueTray: View {
    let fleet: Fleet
    let key: SessionKey
    let queue: [QueuedPrompt]
    /// Whether a turn runs, which the queue waits for.
    let running: Bool
    /// The order a drag left, shown until the machine's queue catches up.
    @State private var moved: [PromptId]?

    #if os(iOS)
    private static let rowHeight: CGFloat = 44
    #else
    private static let rowHeight: CGFloat = 34
    #endif

    var body: some View {
        let shown = Self.ordered(queue, by: moved)
        let mergeable = Self.mergeable(shown)
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: "clock")
                Text(running ? "Queued · runs when this turn ends" : "Queued · runs in this order")
                Spacer(minLength: 0)
                if !mergeable.isEmpty {
                    Button { merge(mergeable) } label: {
                        Label("Merge into one", systemImage: "arrow.triangle.merge")
                            .font(.caption.weight(.medium))
                            .foregroundStyle(Theme.secondary)
                            .padding(.horizontal, 8)
                            .frame(height: 20)
                            .background(Theme.raised, in: .capsule)
                    }
                    .buttonStyle(.plain)
                    .help("Merge your queued messages into one, so they run as one turn")
                }
            }
            .font(.caption)
            .foregroundStyle(Theme.tertiary)
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            List {
                ForEach(Array(shown.enumerated()), id: \.element.promptId) { index, prompt in
                    QueuedRow(fleet: fleet, key: key, prompt: prompt)
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                        .listRowSeparator(.hidden)
                        .frame(height: Self.rowHeight)
                        .overlay(alignment: .top) { Rectangle().fill(Theme.stroke.opacity(0.5)).frame(height: 1) }
                        .contextMenu { menu(prompt, at: index, in: shown, merging: mergeable) }
                        #if os(iOS)
                        .swipeActions(edge: .trailing) {
                            Button(role: .destructive) { remove(prompt) } label: { Label("Remove", systemImage: "trash") }
                                .tint(Theme.failure)
                        }
                        #endif
                }
                .onMove { sources, destination in
                    guard let source = sources.first else { return }
                    move(source, to: destination, in: shown)
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .environment(\.defaultMinListRowHeight, Self.rowHeight)
            // Four messages show at once; more scroll.
            .frame(height: Self.rowHeight * CGFloat(min(shown.count, 4)))
            .scrollDisabled(shown.count <= 4)
        }
        .background(Theme.surface, in: .rect(cornerRadius: 14))
        .clipShape(.rect(cornerRadius: 14))
        .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(Theme.stroke.opacity(0.6)))
        .padding(.horizontal, 12)
        .onChange(of: queue.map(\.promptId)) { moved = nil }
    }

    @ViewBuilder
    private func menu(
        _ prompt: QueuedPrompt, at index: Int, in shown: [QueuedPrompt], merging mergeable: [PromptId]
    ) -> some View {
        Button { Task { await fleet.sendQueuedNow(prompt.promptId, in: key) } } label: {
            Label("Send Now", systemImage: "arrow.up")
        }
        Button { move(index, to: index - 1, in: shown) } label: { Label("Move Up", systemImage: "arrow.up.to.line") }
            .disabled(index == 0)
        Button { move(index, to: index + 2, in: shown) } label: { Label("Move Down", systemImage: "arrow.down.to.line") }
            .disabled(index == shown.count - 1)
        if mergeable.contains(prompt.promptId) {
            Button { merge(mergeable) } label: {
                Label("Merge into One Message", systemImage: "arrow.triangle.merge")
            }
        }
        Divider()
        Button(role: .destructive) { remove(prompt) } label: { Label("Remove", systemImage: "trash") }
    }

    private func remove(_ prompt: QueuedPrompt) {
        Task { await fleet.removeQueued(prompt.promptId, from: key) }
    }

    /// Merges `promptIds` into one on the machine; the tray shows the merged message once its
    /// queue arrives.
    private func merge(_ promptIds: [PromptId]) {
        Task { await fleet.mergeQueued(promptIds, in: key) }
    }

    /// Moves the message at `source` to `destination`, an index in the order before the move,
    /// as `onMove` gives it: at once here, then on the machine.
    private func move(_ source: Int, to destination: Int, in shown: [QueuedPrompt]) {
        let ids = shown.map(\.promptId)
        guard let (promptId, before) = Self.move(source, to: destination, in: ids) else { return }
        var order = ids
        order.move(fromOffsets: [source], toOffset: destination)
        moved = order
        Task {
            await fleet.moveQueued(promptId, before: before, in: key)
            if fleet.refusals[key] != nil { moved = nil }
        }
    }

    /// The `move_queued` that moves `ids[source]` to `destination`, an index in `ids` before the
    /// move: the prompt and the one it is to run before, none to run last; nil when it stays.
    static func move(_ source: Int, to destination: Int, in ids: [PromptId]) -> (PromptId, before: PromptId?)? {
        guard ids.indices.contains(source), (0...ids.count).contains(destination),
              destination != source, destination != source + 1
        else { return nil }
        return (ids[source], destination < ids.count ? ids[destination] : nil)
    }

    /// The messages "Merge into one" merges, in the order shown: those the user who sent the
    /// first of them sent, not another agent's, which keep who sent them; none unless two are.
    static func mergeable(_ shown: [QueuedPrompt]) -> [PromptId] {
        guard let first = shown.first(where: { $0.agentMessage == nil }) else { return [] }
        let ids = shown.filter { $0.agentMessage == nil && $0.by == first.by }.map(\.promptId)
        return ids.count >= 2 ? ids : []
    }

    /// The queue in the order of `moved`, when a drag left one; prompts it lacks go last.
    static func ordered(_ queue: [QueuedPrompt], by moved: [PromptId]?) -> [QueuedPrompt] {
        guard let moved else { return queue }
        return queue.sorted { (moved.firstIndex(of: $0.promptId) ?? .max) < (moved.firstIndex(of: $1.promptId) ?? .max) }
    }
}

/// One queued message: its sender when another agent sent it, its text and images, and Send
/// Now; on the Mac, Send Now and Remove show on hover.
private struct QueuedRow: View {
    let fleet: Fleet
    let key: SessionKey
    let prompt: QueuedPrompt
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 10) {
            #if os(macOS)
            Image(systemName: "line.3.horizontal")
                .font(.caption).foregroundStyle(Theme.tertiary.opacity(hovering ? 1 : 0.5))
                .help("Drag to change when it runs")
            #endif
            VStack(alignment: .leading, spacing: 1) {
                if let message = prompt.agentMessage {
                    Label(fleet.senderTitle(message, on: key.hostId), systemImage: "bubble.left.and.bubble.right")
                        .font(.caption2).foregroundStyle(Theme.tertiary).lineLimit(1)
                        .help("Sent by another agent: \(message.senderSessionId)")
                }
                Text(prompt.text).foregroundStyle(Theme.text).lineLimit(1)
            }
            if prompt.images > 0 {
                Label("\(prompt.images)", systemImage: "photo")
                    .font(.caption).foregroundStyle(Theme.tertiary)
            }
            Spacer(minLength: 0)
            if showsActions {
                Button { Task { await fleet.sendQueuedNow(prompt.promptId, in: key) } } label: {
                    #if os(iOS)
                    // A phone leaves the text the room; the label stays for VoiceOver.
                    Image(systemName: "arrow.up")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Theme.text)
                        .frame(width: 28, height: 28)
                        .background(Theme.raised, in: .circle)
                        .frame(width: 44, height: 44)
                        .contentShape(Rectangle())
                    #else
                    Label("Send Now", systemImage: "arrow.up")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Theme.text)
                        .padding(.horizontal, 10)
                        .frame(height: 24)
                        .background(Theme.raised, in: .capsule)
                    #endif
                }
                .buttonStyle(.plain)
                .help("Stop the running turn and run this message next")
                .accessibilityLabel("Send Now")
                #if os(macOS)
                Button { Task { await fleet.removeQueued(prompt.promptId, from: key) } } label: {
                    Image(systemName: "xmark")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Theme.secondary)
                        .frame(width: 22, height: 22)
                        .background(Theme.raised, in: .circle)
                }
                .buttonStyle(.plain)
                .help("Remove from the queue")
                .accessibilityLabel("Remove")
                #endif
            }
        }
        .font(.subheadline)
        .padding(.horizontal, 14)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }

    private var showsActions: Bool {
        #if os(macOS)
        hovering
        #else
        true
        #endif
    }
}
