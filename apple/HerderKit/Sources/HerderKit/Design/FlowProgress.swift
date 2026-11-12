import SwiftUI

/// Where a guided flow is: its steps in order, those before `current` done. On the Mac a
/// numbered circle per step, a ✓ once done, joined by lines; on the iPhone a bar per step with
/// its name under it. Past the last step every one is done.
struct FlowProgress: View {
    let steps: [String]
    /// The step showing, from 0; `steps.count` once the flow is over.
    let current: Int

    var body: some View {
        #if os(iOS)
        bars
        #else
        circles
        #endif
    }

    /// The Mac's: "① Install —— ② Approve —— ③ Done".
    private var circles: some View {
        HStack(spacing: 10) {
            ForEach(Array(steps.enumerated()), id: \.offset) { index, step in
                if index > 0 {
                    Rectangle().fill(Theme.stroke).frame(width: 36, height: 1)
                }
                HStack(spacing: 8) {
                    mark(index)
                    Text(step)
                        .font(.subheadline.weight(index == current ? .semibold : .regular))
                        .foregroundStyle(index == current ? Theme.text : Theme.secondary)
                }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
    }

    private func mark(_ index: Int) -> some View {
        ZStack {
            if index < current {
                Circle().fill(Theme.switchOn.opacity(0.18))
                Image(systemName: "checkmark").font(.caption2.weight(.heavy)).foregroundStyle(Theme.switchOn)
            } else if index == current {
                Circle().fill(Theme.text)
                Text("\(index + 1)").font(.caption.weight(.bold)).monospacedDigit().foregroundStyle(.ground)
            } else {
                Circle().fill(Theme.raised)
                Text("\(index + 1)").font(.caption.weight(.bold)).monospacedDigit().foregroundStyle(Theme.secondary)
            }
        }
        .frame(width: 22, height: 22)
    }

    /// The iPhone's: a bar per step, filled once reached, with the step's name under it.
    private var bars: some View {
        HStack(alignment: .top, spacing: 6) {
            ForEach(Array(steps.enumerated()), id: \.offset) { index, step in
                VStack(alignment: alignment(index), spacing: 8) {
                    Capsule().fill(index <= current ? Theme.text : Theme.raised).frame(height: 4)
                    Text(step).font(.footnote)
                        .foregroundStyle(index == current ? Theme.text : Theme.tertiary)
                }
                .frame(maxWidth: .infinity, alignment: Alignment(horizontal: alignment(index), vertical: .top))
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
    }

    private func alignment(_ index: Int) -> HorizontalAlignment {
        index == 0 ? .leading : index == steps.count - 1 ? .trailing : .center
    }

    /// "Step 2 of 3: Approve".
    private var label: String {
        current < steps.count ? "Step \(current + 1) of \(steps.count): \(steps[current])" : "All \(steps.count) steps done"
    }
}
