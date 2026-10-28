import SwiftUI

/// The fleet's current toast at the bottom of the window. A success is gone after a few
/// seconds; a failure stays until dismissed, and a tap shows its whole error.
struct ToastView: View {
    let fleet: Fleet
    @State private var details: Toast?

    var body: some View {
        ZStack {
            if let toast = fleet.toast {
                HStack(spacing: 12) {
                    Image(systemName: toast.failed ? "exclamationmark.triangle.fill" : "checkmark.circle.fill")
                        .foregroundStyle(toast.failed ? Theme.failure : Theme.success)
                    Text(toast.text).foregroundStyle(Theme.text).lineLimit(1)
                    if let key = toast.undo {
                        Button("Undo") { Task { await fleet.unarchive(key) } }
                            .buttonStyle(.plain)
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(Theme.text)
                    }
                    #if os(macOS)
                    if let file = toast.reveal {
                        Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([file]) }
                            .buttonStyle(.plain)
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(Theme.text)
                    }
                    #endif
                    if toast.failed {
                        Button("Details") { details = toast }
                            .buttonStyle(.plain)
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(Theme.text)
                        Button { dismiss(toast) } label: {
                            Image(systemName: "xmark")
                                .font(.caption.weight(.bold))
                                .foregroundStyle(Theme.secondary)
                                .hitTarget()
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Dismiss")
                    }
                }
                .font(.subheadline)
                .padding(.horizontal, 16)
                .frame(height: 40)
                .background(Theme.raised, in: .capsule)
                .overlay(Capsule().strokeBorder(Theme.stroke))
                .contentShape(.capsule)
                .onTapGesture { if toast.failed { details = toast } }
                #if os(iOS)
                .gesture(DragGesture(minimumDistance: 12).onEnded {
                    if $0.translation.height > 20 { dismiss(toast) }
                })
                #endif
                .shadow(color: .black.opacity(0.4), radius: 12, y: 4)
                .padding(.horizontal, 16)
                .padding(.bottom, 24)
                .transition(.move(edge: .bottom).combined(with: .opacity))
                .task(id: toast.id) {
                    guard !toast.failed else { return }
                    try? await Task.sleep(for: .seconds(5))
                    dismiss(toast)
                }
            }
        }
        .animation(.snappy, value: fleet.toast)
        .sheet(item: $details) { ToastDetails(toast: $0) }
    }

    private func dismiss(_ toast: Toast) {
        if fleet.toast?.id == toast.id { fleet.toast = nil }
    }
}

/// A failure toast's headline and whole error, selectable and wrapped, with its causes one a line.
private struct ToastDetails: View {
    let toast: Toast
    @Environment(\.dismiss) private var dismiss
    @State private var copied = false

    private var message: String {
        [toast.text, toast.detail.map(chainLines)].compactMap { $0 }.joined(separator: "\n\n")
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Theme.failure)
                Text("Error").font(.headline).foregroundStyle(Theme.text)
                Spacer()
                IconButton(symbol: copied ? "checkmark" : "doc.on.doc", help: copied ? "Copied" : "Copy") {
                    Clipboard.string = message
                    copied = true
                }
                IconButton(symbol: "xmark", help: "Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding(16)
            Rectangle().fill(Theme.stroke).frame(height: 1)
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text(toast.text).font(.body.weight(.semibold)).foregroundStyle(Theme.text)
                    if let detail = toast.detail {
                        Text(chainLines(detail)).font(.callout).foregroundStyle(Theme.secondary)
                    }
                }
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
                .padding(16)
            }
        }
        .layer(.sheet)
        #if os(macOS)
        .frame(minWidth: 420, idealWidth: 560, minHeight: 240, idealHeight: 360)
        #else
        .presentationDetents([.medium, .large])
        #endif
        .task(id: copied) {
            guard copied else { return }
            try? await Task.sleep(for: .seconds(1.5))
            copied = false
        }
    }
}
