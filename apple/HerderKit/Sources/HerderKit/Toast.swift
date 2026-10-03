import SwiftUI

/// "Archiving…" with a spinner, while the machine archives a session.
struct ArchivingLabel: View {
    var body: some View {
        HStack(spacing: 6) {
            ProgressView().controlSize(.small).tint(Theme.secondary)
            Text("Archiving…")
        }
        .font(.caption.weight(.medium))
        .foregroundStyle(Theme.secondary)
        .padding(.horizontal, 10)
        .frame(height: 28)
        .background(Theme.raised, in: .capsule)
    }
}

/// The fleet's current toast at the bottom of the window, gone after a few seconds.
struct ToastView: View {
    let fleet: Fleet

    var body: some View {
        Group {
            if let toast = fleet.toast {
                HStack(spacing: 12) {
                    Image(systemName: "checkmark.circle.fill").foregroundStyle(Theme.success)
                    Text(toast.text).foregroundStyle(Theme.text).lineLimit(1)
                    if let key = toast.undo {
                        Button("Undo") { Task { await fleet.unarchive(key) } }
                            .buttonStyle(.plain)
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(Theme.text)
                    }
                }
                .font(.subheadline)
                .padding(.horizontal, 16)
                .frame(height: 40)
                .background(Theme.raised, in: .capsule)
                .overlay(Capsule().strokeBorder(Theme.stroke))
                .shadow(color: .black.opacity(0.4), radius: 12, y: 4)
                .padding(.bottom, 24)
                .transition(.move(edge: .bottom).combined(with: .opacity))
                .task(id: toast.id) {
                    try? await Task.sleep(for: .seconds(5))
                    if fleet.toast?.id == toast.id { fleet.toast = nil }
                }
            }
        }
        .animation(.snappy, value: fleet.toast)
    }
}
