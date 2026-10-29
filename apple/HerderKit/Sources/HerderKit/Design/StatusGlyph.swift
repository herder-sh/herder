import Herder
import QuartzCore
import SwiftUI

/// A session's state as one small mark: the same set everywhere, so state reads at a glance.
struct StatusGlyph: View {
    let state: SessionState
    var size: CGFloat = 10
    /// Whether running pulses; the sidebar holds still, as the TUI's does.
    var pulses = true

    var body: some View {
        ZStack {
            switch state {
            case .running:
                if pulses { RunningPulse(diameter: size * 2) }
                Circle().fill(Theme.running).frame(width: size, height: size)
            case .needsYou:
                Circle().fill(Theme.accent).frame(width: size + 4, height: size + 4)
                    .overlay(Text("!").font(.system(size: size, weight: .black)).foregroundStyle(.ground))
            case .waiting:
                Circle().strokeBorder(Theme.waiting, style: StrokeStyle(lineWidth: 2, dash: [2, 2]))
                    .frame(width: size + 2, height: size + 2)
            case .idle:
                Circle().strokeBorder(Theme.idle, lineWidth: 1.5).frame(width: size, height: size)
            case .done:
                Image(systemName: "checkmark").font(.system(size: size, weight: .heavy))
                    .foregroundStyle(Theme.success)
            case .error:
                Circle().fill(Theme.failure).frame(width: size + 2, height: size + 2)
                    .overlay(Image(systemName: "xmark").font(.system(size: size * 0.6, weight: .heavy))
                        .foregroundStyle(.ground))
            case .archived:
                Image(systemName: "archivebox").font(.system(size: size)).foregroundStyle(Theme.tertiary)
            case .moved:
                Image(systemName: "arrow.right").font(.system(size: size, weight: .semibold))
                    .foregroundStyle(Theme.tertiary)
            }
        }
        .frame(width: size * 2, height: size * 2)
        .accessibilityLabel(state.label)
    }
}

/// The halo of a running mark: a ring that grows from half its size and fades, over and over.
/// Core Animation runs it on its own. An animation of SwiftUI's that repeats forever, a symbol
/// effect too, renders the whole window again every frame, and with a long transcript open that
/// takes most of the main thread.
struct RunningPulse: View {
    let diameter: CGFloat
    var opacity = 0.35

    var body: some View {
        PulseHalo(color: Theme.running.opacity(opacity)).frame(width: diameter, height: diameter)
    }
}

private struct PulseHalo {
    let color: Color
}

#if os(iOS)
extension PulseHalo: UIViewRepresentable {
    func makeUIView(context: Context) -> PulseHaloView { PulseHaloView() }
    func updateUIView(_ view: PulseHaloView, context: Context) {
        view.color = color.resolve(in: context.environment).cgColor
    }
}

private final class PulseHaloView: UIView {
    private let halo = CAShapeLayer()

    var color: CGColor? {
        didSet { halo.fillColor = color }
    }

    init() {
        super.init(frame: .zero)
        isUserInteractionEnabled = false
        layer.addSublayer(halo)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    override func layoutSubviews() {
        super.layoutSubviews()
        halo.place(in: bounds)
    }

    // Core Animation drops a layer's animations as it leaves the window.
    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window != nil { halo.pulse() }
    }
}
#else
extension PulseHalo: NSViewRepresentable {
    func makeNSView(context: Context) -> PulseHaloView { PulseHaloView() }
    func updateNSView(_ view: PulseHaloView, context: Context) {
        view.color = color.resolve(in: context.environment).cgColor
    }
}

private final class PulseHaloView: NSView {
    private let halo = CAShapeLayer()

    var color: CGColor? {
        didSet { halo.fillColor = color }
    }

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.addSublayer(halo)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    /// Lets clicks fall through to the row the mark is in.
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func layout() {
        super.layout()
        halo.place(in: bounds)
    }

    // Core Animation drops a layer's animations as it leaves the window.
    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window != nil { halo.pulse() }
    }
}
#endif

private extension CAShapeLayer {
    func place(in bounds: CGRect) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        frame = bounds
        path = CGPath(ellipseIn: bounds, transform: nil)
        CATransaction.commit()
    }

    func pulse() {
        let scale = CABasicAnimation(keyPath: "transform.scale")
        scale.fromValue = 0.5
        scale.toValue = 1
        let fade = CABasicAnimation(keyPath: "opacity")
        fade.fromValue = 1
        fade.toValue = 0
        let pulse = CAAnimationGroup()
        pulse.animations = [scale, fade]
        pulse.duration = 1.4
        pulse.timingFunction = CAMediaTimingFunction(name: .easeOut)
        pulse.repeatCount = .infinity
        // Else the app going to the background ends it for good.
        pulse.isRemovedOnCompletion = false
        add(pulse, forKey: "pulse")
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
            Text(verbatim: "#\(pr.number)")
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
        percent >= 90 ? Theme.failure : percent >= 70 ? Theme.limit : Theme.secondary
    }
}
