import SwiftUI

extension View {
    /// Copies `text`, the whole message, which selecting can't: each of its lines is a text of
    /// its own, so a drag stops at a line's end and a long press copies one line. A Mac shows
    /// Copy and Select Text under the message on hover; both offer them in its context menu.
    /// Select Text shows the message in a text view that selects across its lines.
    func messageCopy(_ text: String, alignment: HorizontalAlignment = .leading) -> some View {
        modifier(MessageCopy(text: text, alignment: alignment))
    }

    /// Selectable text on a Mac. On a phone a long press opens the message's menu instead,
    /// which a selectable text would take for its own one-line Copy.
    @ViewBuilder func messageSelection() -> some View {
        #if os(macOS)
        textSelection(.enabled)
        #else
        textSelection(.disabled)
        #endif
    }
}

private struct MessageCopy: ViewModifier {
    let text: String
    let alignment: HorizontalAlignment
    @State private var hovering = false
    @State private var selecting = false

    func body(content: Content) -> some View {
        VStack(alignment: alignment, spacing: 0) {
            content
            #if os(macOS)
            HStack(spacing: 0) {
                CopyButton(text: text)
                Button { selecting = true } label: {
                    Image(systemName: "text.cursor")
                        .foregroundStyle(Theme.secondary)
                        .frame(width: 30, height: 30)
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .help("Select Text")
            }
            .opacity(hovering ? 1 : 0)
            #endif
        }
        .onHover { hovering = $0 }
        .contextMenu {
            Button("Copy", systemImage: "doc.on.doc") { Clipboard.string = text }
            Button("Select Text", systemImage: "text.cursor") { selecting = true }
        }
        .sheet(isPresented: $selecting) { SelectTextSheet(text: text) }
    }
}

/// A message in a text view, to select any part of it.
private struct SelectTextSheet: View {
    let text: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Text("Select Text").font(.headline).foregroundStyle(Theme.text)
                Spacer()
                IconButton(symbol: "doc.on.doc", help: "Copy") { Clipboard.string = text }
                IconButton(symbol: "xmark", help: "Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding(16)
            Rectangle().fill(Theme.stroke).frame(height: 1)
            SelectableText(text: text)
        }
        .background(Theme.surface)
        #if os(macOS)
        .frame(minWidth: 520, idealWidth: 720, minHeight: 360, idealHeight: 560)
        #endif
    }
}

#if os(iOS)
private struct SelectableText: UIViewRepresentable {
    let text: String

    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.isEditable = false
        view.font = .preferredFont(forTextStyle: .body)
        view.adjustsFontForContentSizeCategory = true
        view.textColor = UIColor(Theme.text)
        view.backgroundColor = .clear
        view.textContainerInset = UIEdgeInsets(top: 16, left: 12, bottom: 16, right: 12)
        view.accessibilityIdentifier = "selectable-text"
        return view
    }

    func updateUIView(_ view: UITextView, context: Context) {
        if view.text != text { view.text = text }
    }
}
#else
private struct SelectableText: NSViewRepresentable {
    let text: String

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSTextView.scrollableTextView()
        scroll.drawsBackground = false
        if let view = scroll.documentView as? NSTextView {
            view.isEditable = false
            view.font = .preferredFont(forTextStyle: .body)
            view.textColor = NSColor(Theme.text)
            view.drawsBackground = false
            view.textContainerInset = NSSize(width: 12, height: 16)
            view.setAccessibilityIdentifier("selectable-text")
        }
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        if let view = scroll.documentView as? NSTextView, view.string != text { view.string = text }
    }
}
#endif
