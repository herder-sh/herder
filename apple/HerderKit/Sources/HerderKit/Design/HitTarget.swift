import SwiftUI

extension EnvironmentValues {
    /// The smallest hit area a control takes: 44 points on iOS and iPadOS; none on the Mac,
    /// whose denser 28 to 34 point controls are their own hit areas.
    @Entry var minimumHitSize: CGFloat = {
        #if os(iOS)
        44
        #else
        0
        #endif
    }()
}

extension View {
    /// Pads a control out to the platform's minimum hit area, around its unchanged visible
    /// size. Goes last in a button's label, after its background.
    func hitTarget() -> some View {
        modifier(HitTarget())
    }
}

private struct HitTarget: ViewModifier {
    @Environment(\.minimumHitSize) private var minimum

    func body(content: Content) -> some View {
        content
            .frame(minWidth: minimum, minHeight: minimum)
            .contentShape(.rect)
    }
}
