import SwiftUI

/// Messages waiting for the running turn to end, stacked above the composer in the order they
/// will run; Send Now stops the turn so the first runs at once.
struct QueueTray: View {
    let queued: [Outgoing]
    let sendNow: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: "clock")
                Text("Queued · runs when this turn ends")
                Spacer()
                Button(action: sendNow) {
                    Label("Send Now", systemImage: "arrow.up")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Theme.text)
                        .padding(.horizontal, 10)
                        .frame(height: 24)
                        .background(Theme.raised, in: .capsule)
                }
                #if os(iOS)
                .frame(minWidth: 44, minHeight: 44)
                .contentShape(Rectangle())
                #endif
                .buttonStyle(.plain)
                .help("Stop the running turn so the queue runs now")
            }
            .font(.caption)
            .foregroundStyle(Theme.tertiary)
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            ForEach(queued) { outgoing in
                HStack(spacing: 10) {
                    Text(outgoing.text).foregroundStyle(Theme.text).lineLimit(1)
                    if !outgoing.images.isEmpty {
                        Label("\(outgoing.images.count)", systemImage: "photo")
                            .font(.caption).foregroundStyle(Theme.tertiary)
                    }
                    Spacer(minLength: 0)
                }
                .font(.subheadline)
                .padding(.horizontal, 14)
                .frame(height: 32)
                .overlay(alignment: .top) { Rectangle().fill(Theme.stroke.opacity(0.5)).frame(height: 1) }
            }
        }
        .background(Theme.surface, in: .rect(cornerRadius: 14))
        .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(Theme.stroke.opacity(0.6)))
        .padding(.horizontal, 12)
    }
}
