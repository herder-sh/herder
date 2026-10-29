import SwiftUI

/// A titled group of settings rows, inset in a rounded hairline box; `RowDivider` separates
/// them. `caption` says in a line under the box what the group is for or how it works.
struct SettingsList<Content: View>: View {
    let title: String
    var caption = ""
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: title)
            SettingsGroup { content }
            if !caption.isEmpty {
                Text(LocalizedStringKey(caption)).font(.footnote).foregroundStyle(Theme.tertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

/// The box of a settings group, with no heading: a `SettingsList`'s, a detail sheet's.
struct SettingsGroup<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        VStack(spacing: 0) { content }
            .background(.card, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
    }
}

/// A row that sums something up and opens its details in a sheet (`DetailSheet`): an optional
/// mark, the title, one line of summary, then the value and a chevron. An `accessory`, such as a
/// menu, sits after the row's button in place of the chevron.
struct SummaryRow<Leading: View, Accessory: View>: View {
    let title: String
    var summary = ""
    var value = ""
    var saved: SaveState?
    let open: () -> Void
    @ViewBuilder var leading: Leading
    @ViewBuilder var accessory: Accessory

    var body: some View {
        HStack(spacing: 12) {
            Button(action: open) {
                HStack(spacing: 12) {
                    leading
                    VStack(alignment: .leading, spacing: 2) {
                        Text(title).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text).lineLimit(1)
                        if case .failed(let error) = saved {
                            Text(error).font(.caption).foregroundStyle(Theme.failure).lineLimit(2)
                        } else if !summary.isEmpty {
                            Text(summary).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                        }
                    }
                    .layoutPriority(1)
                    Spacer(minLength: 8)
                    SaveMark(state: saved)
                    if !value.isEmpty {
                        Text(value).font(.subheadline).foregroundStyle(Theme.secondary).lineLimit(1)
                    }
                    if Accessory.self == EmptyView.self {
                        Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                    }
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            accessory
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 8)
        .frame(minHeight: summary.isEmpty ? Theme.rowHeight : Theme.tallRowHeight)
    }
}

extension SummaryRow where Leading == EmptyView, Accessory == EmptyView {
    init(title: String, summary: String = "", value: String = "", saved: SaveState? = nil, open: @escaping () -> Void) {
        self.init(title: title, summary: summary, value: value, saved: saved, open: open,
                  leading: { EmptyView() }, accessory: { EmptyView() })
    }
}

/// The last row of a group that adds to it: "+ Add person…", in the action colour.
struct AddRow: View {
    let title: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Label(title, systemImage: "plus")
                .font(.subheadline.weight(.medium))
                .foregroundStyle(Theme.action)
                .padding(.horizontal, Theme.cellPadding)
                .frame(maxWidth: .infinity, minHeight: Theme.rowHeight, alignment: .leading)
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }
}

/// A line of quiet text in a group with nothing else to show: "Every channel uses the default".
struct EmptyRow: View {
    let text: String

    var body: some View {
        Text(text).font(.subheadline).foregroundStyle(Theme.tertiary)
            .padding(.horizontal, Theme.cellPadding)
            .frame(maxWidth: .infinity, minHeight: Theme.rowHeight, alignment: .leading)
    }
}

/// A switch with its title and the one sentence that says what it does, as a detail sheet
/// lists them.
struct SwitchRow: View {
    let title: String
    var detail = ""
    @Binding var isOn: Bool

    var body: some View {
        Toggle(isOn: $isOn) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                if !detail.isEmpty {
                    Text(detail).font(.caption).foregroundStyle(Theme.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .toggleStyle(.settings)
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 8)
        .frame(minHeight: detail.isEmpty ? Theme.rowHeight : Theme.tallRowHeight)
    }
}

/// herder's switch: green when on, a clearly dark track when off, after its label.
struct SettingsSwitchStyle: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View {
        SettingsSwitchBody(configuration: configuration)
        // Reads as the system's switch, so VoiceOver and UI tests find a switch with its value.
        .accessibilityRepresentation {
            Toggle(isOn: configuration.$isOn) { configuration.label }.toggleStyle(.switch)
        }
    }
}

/// The label, unless hidden (`labelsHidden()`), then the switch. A tap anywhere on it toggles
/// it, as the accessibility element it stands for spans both.
private struct SettingsSwitchBody: View {
    let configuration: ToggleStyleConfiguration
    @Environment(\.labelsVisibility) private var labels

    var body: some View {
        Button { configuration.isOn.toggle() } label: {
            HStack(spacing: 12) {
                if labels != .hidden { configuration.label }
                SettingsSwitch(isOn: configuration.isOn)
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }
}

private struct SettingsSwitch: View {
    let isOn: Bool
    @Environment(\.isEnabled) private var enabled

    var body: some View {
        Capsule()
            .fill(isOn ? Theme.switchOn : Theme.switchOff)
            .frame(width: 34, height: 20)
            .overlay(alignment: isOn ? .trailing : .leading) {
                Circle().fill(.white).frame(width: 16, height: 16).padding(2)
                    .shadow(color: .black.opacity(0.25), radius: 1, y: 0.5)
            }
            .animation(.easeInOut(duration: 0.15), value: isOn)
            .opacity(enabled ? 1 : 0.45)
            .hitTarget()
    }
}

extension ToggleStyle where Self == SettingsSwitchStyle {
    static var settings: SettingsSwitchStyle { SettingsSwitchStyle() }
}

/// A person's initials on a fill picked from their id, so it stays theirs on every device and
/// every launch.
struct Avatar: View {
    let name: String
    let id: String
    var size: CGFloat = 28

    var body: some View {
        Text(Self.initials(name))
            .font(.system(size: size * 0.43, weight: .semibold))
            .foregroundStyle(.white)
            .frame(width: size, height: size)
            .background(Theme.avatars[Self.colorIndex(id)], in: .circle)
            .accessibilityHidden(true)
    }

    /// The first letter of the name's first word, and of its last when it has more than one.
    nonisolated static func initials(_ name: String) -> String {
        let words = name.split(whereSeparator: \.isWhitespace)
        let letters = [words.first, words.count > 1 ? words.last : nil].compactMap { $0?.first }
        return letters.isEmpty ? "?" : String(letters).uppercased()
    }

    /// Which of `Theme.avatars` fills `id`'s circle: FNV-1a of its bytes, which unlike
    /// `hashValue` is the same on every launch, then mixed so ids that differ in a letter or
    /// two (U04SAM0001, U04KIM0003) still spread across the fills.
    nonisolated static func colorIndex(_ id: String) -> Int {
        var hash: UInt32 = 2_166_136_261
        for byte in id.utf8 {
            hash = (hash ^ UInt32(byte)) &* 16_777_619
        }
        hash ^= hash >> 16
        hash = hash &* 0x7FEB_352D
        hash ^= hash >> 15
        hash = hash &* 0x846C_A68B
        hash ^= hash >> 16
        return Int(hash % UInt32(Theme.avatars.count))
    }
}

/// A settings sheet's button: `primary` in the action colour, `destructive` red text, `quiet`
/// only a hairline, else plain on the raised surface.
struct SettingsButton: View {
    enum Style { case primary, plain, quiet, destructive }
    let title: String
    var style = Style.plain
    let action: () -> Void
    @Environment(\.isEnabled) private var enabled

    var body: some View {
        Button(role: style == .destructive ? .destructive : nil, action: action) {
            Text(title)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(style == .primary ? .white : style == .destructive ? Theme.danger : Theme.text)
                .padding(.horizontal, 14)
                .frame(minWidth: style == .primary ? 72 : 0, minHeight: 32)
                .background(style == .primary ? Theme.action : style == .plain ? Theme.raised : .clear,
                            in: .rect(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8)
                    .strokeBorder(style == .destructive || style == .quiet ? Theme.stroke : .clear))
                .opacity(enabled ? 1 : 0.45)
                .contentShape(.rect)
                .hitTarget()
        }
        .buttonStyle(.plain)
    }
}

/// The details behind a `SummaryRow`: a title with a monospaced id under it when there is one,
/// the rows, then a destructive action at the bottom left and the primary one, Done unless
/// said otherwise, at the right. Changes made in it apply at once, as on its page.
struct DetailSheet<Content: View>: View {
    let title: String
    var id = ""
    var destructive = ""
    var onDestructive: () -> Void = {}
    var primary = "Done"
    /// The primary button's action; closing the sheet when not given.
    var onPrimary: (() -> Void)?
    var primaryDisabled = false
    @ViewBuilder var content: Content
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(title).font(.title3.weight(.bold)).foregroundStyle(Theme.text).lineLimit(1)
                if !id.isEmpty {
                    Text(id).font(Theme.monoSmall).foregroundStyle(Theme.tertiary).textSelection(.enabled)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 20)
            .padding(.top, 20)
            .padding(.bottom, 14)
            ScrollView {
                VStack(alignment: .leading, spacing: 14) { content }
                    .padding(.horizontal, 20)
                    .padding(.bottom, 4)
            }
            .scrollBounceBehavior(.basedOnSize)
            HStack(spacing: 10) {
                if !destructive.isEmpty {
                    SettingsButton(title: destructive, style: .destructive, action: onDestructive)
                }
                Spacer()
                SettingsButton(title: primary, style: .primary) { (onPrimary ?? { dismiss() })() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(primaryDisabled)
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 16)
        }
        .layer(.sheet)
        // Escape closes it, as a sheet's close button does.
        .background { Button("Close") { dismiss() }.keyboardShortcut(.cancelAction).hidden() }
        #if os(macOS)
        .frame(width: 480)
        .frame(minHeight: 200, idealHeight: 420)
        #else
        .presentationDetents([.medium, .large])
        .presentationBackground(Theme.sheet)
        #endif
        .preferredColorScheme(.dark)
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
    .background(Theme.canvas)
}
