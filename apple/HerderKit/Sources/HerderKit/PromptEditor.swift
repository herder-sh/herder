#if os(macOS)
import AppKit
import Herder
import SwiftUI

/// The composer's text view on the Mac: plain text whose markers ([`PromptText`]) draw as chips,
/// each opening what it stands for. It grows from two lines to twelve, then scrolls.
struct PromptEditor: NSViewRepresentable {
    @Binding var text: String
    @Binding var focused: Bool
    let images: [Herder.Image]
    let pastes: [String]
    /// Attaches pasted or dropped images; returns their markers, to go in at the caret.
    let addImages: ([Herder.Image]) -> String
    /// Takes long pasted text; returns its marker, to go in at the caret.
    let addPaste: (String) -> String
    /// Return without Shift.
    let submit: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let view = ChipTextView(usingTextLayoutManager: true)
        view.coordinator = context.coordinator
        view.delegate = context.coordinator
        view.isRichText = false
        view.importsGraphics = false
        view.allowsUndo = true
        view.drawsBackground = false
        view.font = Self.font
        view.textColor = NSColor(Theme.text)
        view.insertionPointColor = NSColor(Theme.text)
        view.typingAttributes = Self.attributes
        view.textContainerInset = .zero
        view.textContainer?.lineFragmentPadding = 0
        view.isVerticallyResizable = true
        view.isHorizontallyResizable = false
        view.autoresizingMask = [.width]
        view.textContainer?.widthTracksTextView = true
        view.setAccessibilityIdentifier("composer")
        view.registerForDraggedTypes([.fileURL, .png, .tiff])
        let scroll = NSScrollView()
        scroll.documentView = view
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        context.coordinator.view = view
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self
        guard let view = coordinator.view else { return }
        if coordinator.text(of: view) != text {
            coordinator.show(text, in: view)
        }
        if focused, view.window?.firstResponder !== view {
            DispatchQueue.main.async { view.window?.makeFirstResponder(view) }
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: NSScrollView, context: Context) -> CGSize? {
        guard let view = context.coordinator.view, let width = proposal.width, width > 0 else { return nil }
        let line = Self.font.boundingRectForFont.height
        view.frame.size.width = width
        guard let layout = view.textLayoutManager else { return nil }
        layout.ensureLayout(for: layout.documentRange)
        let used = layout.usageBoundsForTextContainer.height
        return CGSize(width: width, height: min(max(used, line * 2), line * 12).rounded(.up))
    }

    static let font = NSFont.preferredFont(forTextStyle: .body)
    static var attributes: [NSAttributedString.Key: Any] {
        [.font: font, .foregroundColor: NSColor(Theme.text)]
    }

    @MainActor final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: PromptEditor
        weak var view: ChipTextView?

        init(_ parent: PromptEditor) { self.parent = parent }

        /// The text as the binding holds it: each chip back to its marker.
        func text(of view: NSTextView) -> String {
            let storage = view.attributedString()
            var out = ""
            storage.enumerateAttribute(.attachment, in: NSRange(location: 0, length: storage.length)) { value, range, _ in
                if let chip = value as? ChipAttachment {
                    out += chip.token.marker
                } else {
                    out += (storage.string as NSString).substring(with: range)
                }
            }
            return out
        }

        /// Shows `text`, its markers as chips, keeping the caret where it was or at the end.
        func show(_ text: String, in view: NSTextView) {
            let shown = NSMutableAttributedString()
            var rest = text.startIndex
            for (range, token) in PromptText.tokens(in: text) {
                shown.append(NSAttributedString(string: String(text[rest..<range.lowerBound]), attributes: PromptEditor.attributes))
                let dark = view.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                let attachment = ChipAttachment(token: token, label: ChipLabel(chip: chip(token)), dark: dark)
                let marker = NSMutableAttributedString(attachment: attachment)
                marker.addAttributes(PromptEditor.attributes, range: NSRange(location: 0, length: marker.length))
                shown.append(marker)
                rest = range.upperBound
            }
            shown.append(NSAttributedString(string: String(text[rest...]), attributes: PromptEditor.attributes))
            let caret = view.selectedRange()
            view.textStorage?.setAttributedString(shown)
            let end = shown.length
            view.setSelectedRange(caret.location <= end && caret.location > 0 ? NSRange(location: min(caret.location, end), length: 0) : NSRange(location: end, length: 0))
        }

        /// Inserts text with markers at the caret, as typing would.
        func insert(_ text: String, in view: NSTextView) {
            guard !text.isEmpty else { return }
            view.insertText(text, replacementRange: view.selectedRange())
            parent.text = self.text(of: view)
            show(parent.text, in: view)
        }

        func textDidChange(_ notification: Notification) {
            guard let view else { return }
            parent.text = text(of: view)
        }

        func textDidBeginEditing(_ notification: Notification) { parent.focused = true }
        func textDidEndEditing(_ notification: Notification) { parent.focused = false }

        func textView(_ textView: NSTextView, doCommandBy selector: Selector) -> Bool {
            guard selector == #selector(NSResponder.insertNewline(_:)) else { return false }
            if NSApp.currentEvent?.modifierFlags.contains(.shift) == true {
                let before = text(of: textView)
                let caret = textView.selectedRange()
                if caret.location == (textView.string as NSString).length {
                    parent.text = ListContinuation.newline(after: before)
                    show(parent.text, in: textView)
                } else {
                    textView.insertText("\n", replacementRange: caret)
                }
                return true
            }
            parent.submit()
            return true
        }

        func chip(_ token: PromptText.Token) -> PromptChip {
            PromptChip(token: token, image: token.kind == .image ? parent.images[safe: token.number - 1] : nil,
                 paste: token.kind == .paste ? parent.pastes[safe: token.number - 1] : nil)
        }

        /// Opens what a chip stands for, under it.
        func open(_ token: PromptText.Token, at rect: NSRect, in view: NSView) {
            let popover = NSPopover()
            popover.behavior = .transient
            popover.contentViewController = NSHostingController(rootView: ChipDetail(chip: chip(token)) { [weak self, weak popover] in
                popover?.close()
                guard let self, let view = self.view else { return }
                self.parent.text = self.parent.text.replacingOccurrences(of: token.marker, with: "")
                self.show(self.parent.text, in: view)
            })
            popover.show(relativeTo: rect, of: view, preferredEdge: .maxY)
        }
    }
}

/// The text view: pastes and drops of images become chips, as do long pastes of text.
final class ChipTextView: NSTextView {
    weak var coordinator: PromptEditor.Coordinator?

    /// A plain text view enables Paste only for text; images paste here too.
    override func validateUserInterfaceItem(_ item: any NSValidatedUserInterfaceItem) -> Bool {
        if item.action == #selector(paste(_:)), ImageAttachment.available() { return true }
        return super.validateUserInterfaceItem(item)
    }

    override var readablePasteboardTypes: [NSPasteboard.PasteboardType] {
        super.readablePasteboardTypes + [.png, .tiff, .fileURL]
    }

    override func paste(_ sender: Any?) {
        guard let coordinator else { return super.paste(sender) }
        let images = ImageAttachment.from()
        if !images.isEmpty {
            coordinator.insert(coordinator.parent.addImages(images), in: self)
        } else if let pasted = NSPasteboard.general.string(forType: .string), PromptText.isLong(pasted) {
            coordinator.insert(coordinator.parent.addPaste(pasted), in: self)
        } else {
            pasteAsPlainText(sender)
        }
    }

    /// A click on a chip opens it; anywhere else edits as usual.
    override func mouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        let index = characterIndexForInsertion(at: point)
        let storage = attributedString()
        for candidate in [index, index - 1] where candidate >= 0 && candidate < storage.length {
            guard let chip = storage.attribute(.attachment, at: candidate, effectiveRange: nil) as? ChipAttachment,
                  let rect = rect(ofCharacterAt: candidate), rect.contains(point) else { continue }
            coordinator?.open(chip.token, at: rect, in: self)
            return
        }
        super.mouseDown(with: event)
    }

    /// Where a character is drawn, in this view.
    private func rect(ofCharacterAt index: Int) -> NSRect? {
        guard let window else { return nil }
        let screen = firstRect(forCharacterRange: NSRange(location: index, length: 1), actualRange: nil)
        guard screen != .zero else { return nil }
        return convert(window.convertFromScreen(screen), from: nil)
    }

    override func performDragOperation(_ sender: any NSDraggingInfo) -> Bool {
        guard let coordinator else { return super.performDragOperation(sender) }
        let images = ImageAttachment.from(sender.draggingPasteboard)
        guard !images.isEmpty else { return super.performDragOperation(sender) }
        coordinator.insert(coordinator.parent.addImages(images), in: self)
        return true
    }
}

/// A marker drawn as a chip: its label rendered as the attachment's image.
final class ChipAttachment: NSTextAttachment {
    let token: PromptText.Token

    @MainActor init(token: PromptText.Token, label: ChipLabel, dark: Bool) {
        self.token = token
        super.init(data: nil, ofType: nil)
        let renderer = ImageRenderer(content: label.environment(\.colorScheme, dark ? .dark : .light))
        renderer.scale = NSScreen.main?.backingScaleFactor ?? 2
        if let image = renderer.nsImage {
            self.image = image
            // On the text's baseline, like a word.
            bounds = CGRect(x: 0, y: PromptEditor.font.descender - 3, width: image.size.width, height: image.size.height)
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not decoded") }
}

/// What a marker stands for, as its chip and its popover show it.
struct PromptChip {
    let token: PromptText.Token
    let image: Herder.Image?
    let paste: String?

    var name: String {
        switch token.kind {
        case .image:
            let ext = image.flatMap { $0.mediaType.split(separator: "/").last.map(String.init) } ?? "png"
            return token.number == 1 ? "image.\(ext)" : "image-\(token.number).\(ext)"
        case .paste:
            return token.number == 1 ? "pasted-text.txt" : "pasted-text-\(token.number).txt"
        }
    }

    var detail: String {
        switch token.kind {
        case .image:
            return ByteCountFormatter.string(fromByteCount: Int64(image?.data.count ?? 0), countStyle: .file)
        case .paste:
            var text = paste ?? ""
            if text.hasSuffix("\n") { text.removeLast() }
            let lines = text.split(separator: "\n", omittingEmptySubsequences: false).count
            return lines == 1 ? "1 line" : "\(lines) lines"
        }
    }
}

/// A chip in the text: an icon or thumbnail, the name and the size.
struct ChipLabel: View {
    let chip: PromptChip

    var body: some View {
        HStack(spacing: 5) {
            if let image = chip.image, let picture = NSImage(data: image.data) {
                SwiftUI.Image(nsImage: picture).resizable().scaledToFill()
                    .frame(width: 14, height: 14).clipShape(.rect(cornerRadius: 3))
            } else {
                SwiftUI.Image(systemName: "doc.text").foregroundStyle(Theme.secondary)
            }
            Text(chip.name).foregroundStyle(Theme.text)
            Text(chip.detail).foregroundStyle(Theme.tertiary)
        }
        .font(.callout)
        .lineLimit(1)
        .padding(.horizontal, 7)
        .padding(.vertical, 2)
        .background(Theme.raised, in: .rect(cornerRadius: 6))
        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(Theme.stroke))
    }
}

/// What a chip stands for, in the popover a click on it opens.
struct ChipDetail: View {
    let chip: PromptChip
    let remove: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text(chip.name).font(.headline).foregroundStyle(Theme.text)
                Text(chip.detail).foregroundStyle(Theme.tertiary)
                Spacer()
                Button("Remove", role: .destructive, action: remove)
            }
            if let image = chip.image {
                Picture(data: image.data, height: 360)
            } else if let paste = chip.paste {
                ScrollView {
                    Text(paste).font(Theme.mono).foregroundStyle(Theme.text).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(width: 560, height: 360)
            }
        }
        .padding(14)
        .frame(minWidth: 320)
        .background(Theme.surface)
    }
}

extension Array {
    subscript(safe index: Int) -> Element? { indices.contains(index) ? self[index] : nil }
}
#endif
