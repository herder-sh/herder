import Herder
import SwiftUI

/// A session's state as one small mark: the same set everywhere, so state reads at a glance.
struct StatusGlyph: View {
    let state: SessionState
    var size: CGFloat = 10
    @State private var pulsing = false

    var body: some View {
        ZStack {
            switch state {
            case .running:
                Circle().fill(Theme.running.opacity(0.35))
                    .frame(width: size * 2, height: size * 2)
                    .scaleEffect(pulsing ? 1 : 0.5)
                    .opacity(pulsing ? 0 : 1)
                    .animation(.easeOut(duration: 1.4).repeatForever(autoreverses: false), value: pulsing)
                Circle().fill(Theme.running).frame(width: size, height: size)
            case .needsYou:
                Circle().fill(Theme.accent).frame(width: size + 4, height: size + 4)
                    .overlay(Text("!").font(.system(size: size, weight: .black)).foregroundStyle(Theme.background))
            case .waiting:
                Circle().strokeBorder(Theme.waiting, style: StrokeStyle(lineWidth: 2, dash: [2, 2]))
                    .frame(width: size + 2, height: size + 2)
            case .idle:
                Circle().strokeBorder(Theme.idle, lineWidth: 1.5).frame(width: size, height: size)
            case .error:
                Circle().fill(Theme.failure).frame(width: size + 2, height: size + 2)
                    .overlay(Image(systemName: "xmark").font(.system(size: size * 0.6, weight: .heavy))
                        .foregroundStyle(Theme.background))
            case .archived:
                Image(systemName: "archivebox").font(.system(size: size)).foregroundStyle(Theme.tertiary)
            case .moved:
                Image(systemName: "arrow.right").font(.system(size: size, weight: .semibold))
                    .foregroundStyle(Theme.tertiary)
            }
        }
        .frame(width: size * 2, height: size * 2)
        .onAppear { pulsing = true }
        .accessibilityLabel(state.label)
    }
}

/// A PR as a compact badge: number coloured by state; open and draft PRs add a CI mark and a
/// warning for conflicts or requested changes, as the TUI's badge does.
struct PRBadge: View {
    let pr: PullRequest

    var body: some View {
        HStack(spacing: 3) {
            Image(systemName: pr.state == .merged ? "arrow.triangle.merge" : "arrow.triangle.pull")
                .imageScale(.small)
            Text("#\(pr.number)")
            if pr.state == .open || pr.state == .draft {
                switch pr.ci {
                case .passing: Image(systemName: "checkmark").foregroundStyle(Theme.success)
                case .failing: Image(systemName: "xmark").foregroundStyle(Theme.failure)
                case .pending: Image(systemName: "circle.dotted").foregroundStyle(Theme.accent)
                case .none: EmptyView()
                }
                if pr.mergeable == .conflicting || pr.review == .changesRequested {
                    Text("!").foregroundStyle(Theme.failure)
                }
            }
        }
        .font(.caption2.weight(.semibold).monospaced())
        .lineLimit(1)
        .fixedSize()
        .foregroundStyle(pr.state.color)
        .padding(.horizontal, 6)
        .padding(.vertical, 3)
        .background(pr.state.color.opacity(0.14), in: .capsule)
    }
}

extension PrState {
    var color: Color {
        switch self {
        case .open: Theme.success
        case .draft: Theme.secondary
        case .merged: Theme.merged
        case .closed: Theme.failure
        }
    }
}

/// A thin usage bar: neutral below 70%, accent from 70%, red from 90%, as the TUI colours it.
struct UsageBar: View {
    let percent: Double
    var height: CGFloat = 6

    var body: some View {
        GeometryReader { geometry in
            ZStack(alignment: .leading) {
                Capsule().fill(Theme.raised)
                Capsule().fill(color)
                    .frame(width: geometry.size.width * min(max(percent, 0), 100) / 100)
            }
        }
        .frame(height: height)
    }

    private var color: Color {
        percent >= 90 ? Theme.failure : percent >= 70 ? Theme.accent : Theme.secondary
    }
}
