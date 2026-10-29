import SwiftUI

/// A full-width button of an approval-style sheet: the action colour, or quiet with a hairline in
/// `tint` (Theme.danger for "Not mine").
struct WideButton: View {
    let title: String
    var primary = false
    var tint = Theme.text
    let action: () -> Void
    @Environment(\.isEnabled) private var enabled

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(Theme.body.weight(.semibold))
                .foregroundStyle(primary ? .white : tint)
                .frame(maxWidth: .infinity, minHeight: 42)
                .background(primary ? Theme.action : .clear, in: .rect(cornerRadius: 9))
                .overlay(RoundedRectangle(cornerRadius: 9).strokeBorder(primary ? Theme.action : Theme.stroke))
                .opacity(enabled ? 1 : 0.45)
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }
}
