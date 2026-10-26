import SwiftUI

/// A titled group of settings rows on the raised surface; `RowDivider` separates them.
struct SettingsList<Content: View>: View {
    let title: String
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: title)
            VStack(spacing: 0) {
                content
            }
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
        }
    }
}

/// The hairline between rows of a settings group.
struct RowDivider: View {
    var body: some View {
        Rectangle().fill(Theme.stroke.opacity(0.6)).frame(height: 1).padding(.leading, Theme.cellPadding)
    }
}

/// A setting: its name and at most one line of hint on the left, its control on the right.
/// Longer help goes behind an ⓘ beside the name. On a page that saves as it goes, `saved` puts
/// a quiet "Saved" by the control, or the error in the hint's place (`Autosave`).
struct SettingRow<Control: View>: View {
    let label: String
    var hint = ""
    var help = ""
    var saved: SaveState?
    @ViewBuilder var control: Control

    var body: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Text(label).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                    if !help.isEmpty { InfoButton(text: help) }
                }
                if case .failed(let error) = saved {
                    Text(error).font(.caption).foregroundStyle(Theme.failure).lineLimit(2)
                } else if !hint.isEmpty {
                    Text(hint).font(.caption).foregroundStyle(Theme.tertiary).lineLimit(1)
                }
            }
            Spacer(minLength: 12)
            SaveMark(state: saved)
            control
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 8)
        .frame(minHeight: Theme.tallRowHeight)
    }
}

/// An ⓘ that shows `text` in a popover: the place for what a setting or page is for.
struct InfoButton: View {
    let text: String
    @State private var showing = false

    var body: some View {
        Button { showing.toggle() } label: {
            Image(systemName: "info.circle")
                .font(.caption)
                .foregroundStyle(Theme.tertiary)
                .hitTarget()
        }
        .buttonStyle(.plain)
        .help(text)
        .accessibilityLabel("About this")
        .popover(isPresented: $showing) {
            Text(text)
                .font(Theme.body)
                .foregroundStyle(Theme.text)
                .fixedSize(horizontal: false, vertical: true)
                .frame(width: 280, alignment: .leading)
                .padding(Theme.cellPadding)
                .presentationCompactAdaptation(.popover)
        }
    }
}

#Preview("SettingsList") {
    SettingsList(title: "Project") {
        SettingRow(label: "Name", hint: "Blank goes back to the repository's") {
            Text("herder").foregroundStyle(Theme.secondary)
        }
        RowDivider()
        SettingRow(label: "Account", hint: "Rotates to another when it runs out",
                   help: "herder moves a turn to the provider's next account only after this one hits its limit, never before.") {
            Text("Any").foregroundStyle(Theme.secondary)
        }
        RowDivider()
        SettingRow(label: "Keep private") { Toggle("", isOn: .constant(true)).labelsHidden() }
    }
    .frame(width: 520)
    .padding()
    .background(Theme.surface)
}
