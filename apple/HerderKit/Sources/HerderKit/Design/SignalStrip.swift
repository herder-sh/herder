import SwiftUI

/// How urgent a signal is, which sets its colour. Only `needsYou` is amber.
enum Urgency {
    case needsYou, failing, running, done, quiet

    var color: Color {
        switch self {
        case .needsYou: Theme.accent
        case .failing: Theme.failure
        case .running: Theme.running
        case .done: Theme.success
        case .quiet: Theme.secondary
        }
    }
}

/// One count a page can filter by: "2 need you", "3 running".
struct Signal: Identifiable, Equatable {
    let id: String
    let count: Int
    /// What the count is of, after the number: "need you".
    let label: String
    let urgency: Urgency
}

/// A page's signal strip: a chip for each state that is non-zero, coloured by urgency, in the
/// order given, wrapping where they do not fit. A chip filters the page to its state; the selected one clears it. Zeros never
/// show, and with nothing to show the strip takes no room.
struct SignalStrip: View {
    let signals: [Signal]
    @Binding var filter: Signal.ID?

    var body: some View {
        let shown = Self.shown(signals)
        if !shown.isEmpty {
            FlowLayout(spacing: 6) {
                ForEach(shown) { signal in chip(signal) }
            }
        }
    }

    static func shown(_ signals: [Signal]) -> [Signal] { signals.filter { $0.count > 0 } }

    /// The filter after a tap on `id`: that signal, or none when it was already chosen.
    static func toggled(_ filter: Signal.ID?, _ id: Signal.ID) -> Signal.ID? { filter == id ? nil : id }

    private func chip(_ signal: Signal) -> some View {
        let selected = filter == signal.id
        let tint = signal.urgency.color
        return Button { filter = Self.toggled(filter, signal.id) } label: {
            Text("\(signal.count) \(signal.label)")
                .font(Theme.meta.weight(.medium).monospacedDigit())
                .foregroundStyle(tint)
                .lineLimit(1)
                .padding(.horizontal, 8)
                .padding(.vertical, 3)
                .background(tint.opacity(selected ? 0.28 : 0.12), in: .capsule)
                .overlay(Capsule().strokeBorder(tint.opacity(selected ? 0.9 : 0.35)))
                .hitTarget()
        }
        .buttonStyle(.plain)
        .help(selected ? "Show all" : "Show only \(signal.label)")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

#Preview("SignalStrip") {
    @Previewable @State var filter: Signal.ID? = "running"
    SignalStrip(signals: [
        Signal(id: "needs", count: 2, label: "need you", urgency: .needsYou),
        Signal(id: "failed", count: 1, label: "failed", urgency: .failing),
        Signal(id: "running", count: 3, label: "running", urgency: .running),
        Signal(id: "moved", count: 0, label: "moved", urgency: .quiet),
        Signal(id: "idle", count: 49, label: "idle", urgency: .quiet),
    ], filter: $filter)
    .padding()
    .background(Theme.background)
}
