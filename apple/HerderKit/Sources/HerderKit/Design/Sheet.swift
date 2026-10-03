import SwiftUI

/// A sheet in herder's style: a header with a close button, scrolling content, and a footer
/// with the primary action.
struct SheetScaffold<Content: View, Footer: View>: View {
    let title: String
    var subtitle = ""
    /// The sheet's height on the Mac.
    var height: CGFloat = 620
    @ViewBuilder var content: Content
    @ViewBuilder var footer: Footer
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 3) {
                    Text(title).font(.title3.weight(.bold)).foregroundStyle(Theme.text)
                    if !subtitle.isEmpty {
                        Text(subtitle).font(.subheadline).foregroundStyle(Theme.secondary)
                    }
                }
                Spacer()
                IconButton(symbol: "xmark", help: "Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding(20)
            ScrollView {
                VStack(alignment: .leading, spacing: 18) { content }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 20)
                    .padding(.bottom, 20)
            }
            Rectangle().fill(Theme.stroke).frame(height: 1)
            HStack(spacing: 10) { footer }
                .padding(16)
        }
        .background(Theme.surface)
        #if os(macOS)
        .frame(width: 560, height: height)
        #endif
        .preferredColorScheme(.dark)
    }
}

/// A labelled form field: a small heading, the control, and an optional hint.
struct Field<Control: View>: View {
    let label: String
    var hint = ""
    @ViewBuilder var control: Control

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: label)
            control
            if !hint.isEmpty {
                Text(LocalizedStringKey(hint)).font(.footnote).foregroundStyle(Theme.tertiary)
            }
        }
    }
}

/// A text input on a raised surface.
struct InputBox: View {
    let placeholder: String
    @Binding var text: String
    var mono = false
    var lines: ClosedRange<Int> = 1...1

    var body: some View {
        TextField(placeholder, text: $text, axis: lines.upperBound > 1 ? .vertical : .horizontal)
            .textFieldStyle(.plain)
            .font(mono ? Theme.mono : .body)
            .foregroundStyle(Theme.text)
            .lineLimit(lines)
            .autocorrectionDisabled()
            #if os(iOS)
            .textInputAutocapitalization(.never)
            #endif
            .padding(12)
            .frame(minHeight: 44)
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// One-of-several choice as large chips; wraps onto more lines when needed.
struct ChoiceChips<Value: Hashable>: View {
    let options: [(value: Value, label: String, detail: String)]
    @Binding var selection: Value

    var body: some View {
        FlowLayout(spacing: 8) {
            ForEach(options, id: \.value) { option in
                let selected = option.value == selection
                Button { selection = option.value } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(option.label).font(.subheadline.weight(.semibold))
                        if !option.detail.isEmpty {
                            Text(option.detail).font(.caption).opacity(0.7)
                        }
                    }
                    .foregroundStyle(selected ? Theme.onPrimary : Theme.text)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .frame(minHeight: 44)
                    .background(selected ? Theme.primary : Theme.raised, in: .rect(cornerRadius: Theme.corner))
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
            }
        }
    }
}

/// A row of label and value, for read-only details.
struct DetailRow: View {
    let label: String
    let value: String
    var mono = false

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text(label).foregroundStyle(Theme.secondary).frame(width: 120, alignment: .leading)
            Text(value.isEmpty ? "—" : value)
                .font(mono ? Theme.mono : .subheadline)
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .font(.subheadline)
    }
}

/// Lays children out left to right, wrapping onto new lines.
struct FlowLayout: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let rows = rows(width: proposal.width ?? .infinity, subviews: subviews)
        let height = rows.map(\.height).reduce(0, +) + spacing * CGFloat(max(rows.count - 1, 0))
        return CGSize(width: proposal.width ?? rows.map(\.width).max() ?? 0, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for row in rows(width: bounds.width, subviews: subviews) {
            var x = bounds.minX
            for index in row.indices {
                let size = subviews[index].sizeThatFits(.unspecified)
                subviews[index].place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
                x += size.width + spacing
            }
            y += row.height + spacing
        }
    }

    private func rows(width: CGFloat, subviews: Subviews) -> [(indices: [Int], width: CGFloat, height: CGFloat)] {
        var rows: [(indices: [Int], width: CGFloat, height: CGFloat)] = []
        var current: (indices: [Int], width: CGFloat, height: CGFloat) = ([], 0, 0)
        for index in subviews.indices {
            let size = subviews[index].sizeThatFits(.unspecified)
            let next = current.indices.isEmpty ? size.width : current.width + spacing + size.width
            if next > width && !current.indices.isEmpty {
                rows.append(current)
                current = ([index], size.width, size.height)
            } else {
                current = (current.indices + [index], next, max(current.height, size.height))
            }
        }
        if !current.indices.isEmpty { rows.append(current) }
        return rows
    }
}
