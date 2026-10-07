import Herder
import SwiftUI
import UniformTypeIdentifiers
#if os(macOS)
import AppKit
#else
import GameController
import UIKit
#endif

#if os(macOS)
/// The composer's text view on the Mac: plain text whose markers ([`PromptText`]) draw as chips,
/// each opening what it stands for. It grows from two lines to twelve, then scrolls.
struct PromptEditor: NSViewRepresentable {
    @Binding var text: String
    @Binding var focused: Bool
    let images: [Herder.Image]
    let pastes: [String]
    /// The session's skills, whose `$name` mentions draw as chips.
    var skills: [SessionSkill] = []
    /// Attaches pasted or dropped images; returns their markers, to go in at the caret.
    let addImages: ([Herder.Image]) -> String
    /// Attaches pasted or dropped files that are not pictures.
    let addFiles: ([URL]) -> Void
    /// Takes long pasted text; returns its marker, to go in at the caret.
    let addPaste: (String) -> String
    /// Return without Shift.
    let submit: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let view = ChipTextView(usingTextLayoutManager: true)
        context.coordinator.attach(view)
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
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self
        guard let view = coordinator.view else { return }
        coordinator.refresh(view)
        if focused, view.window?.firstResponder !== view {
            DispatchQueue.main.async { view.window?.makeFirstResponder(view) }
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: NSScrollView, context: Context) -> CGSize? {
        guard let view = context.coordinator.view, let width = proposal.width, width > 0 else { return nil }
        let line = Self.font.boundingRectForFont.height
        let used = Self.height(of: view.attributedString(), width: width)
        return CGSize(width: width, height: min(max(used, line * 2), line * 12).rounded(.up))
    }

    /// The height `text` takes at `width`, laid out on its own: resizing the view itself while
    /// SwiftUI measures it moves whatever is anchored to it, like a chip's popover, mid-update.
    static func height(of text: NSAttributedString, width: CGFloat) -> CGFloat {
        let storage = NSTextStorage(attributedString: text)
        let container = NSTextContainer(size: CGSize(width: width, height: .greatestFiniteMagnitude))
        container.lineFragmentPadding = 0
        let layout = NSLayoutManager()
        layout.addTextContainer(container)
        storage.addLayoutManager(layout)
        layout.ensureLayout(for: container)
        return layout.usedRect(for: container).height
    }

    static let font = NSFont.preferredFont(forTextStyle: .body)
    static var attributes: [NSAttributedString.Key: Any] {
        [.font: font, .foregroundColor: NSColor(Theme.text)]
    }

    @MainActor final class Coordinator: NSObject, NSTextViewDelegate, NSTextStorageDelegate {
        var parent: PromptEditor
        weak var view: ChipTextView?
        /// While `show` replaces the text: the binding already holds it.
        private var showing = false
        /// The images, pastes and skills the chips were last drawn with.
        private var drawn: Content?

        struct Content: Equatable {
            let images: [Data]
            let pastes: [String]
            let skills: [SessionSkill]
        }

        private var content: Content {
            Content(images: parent.images.map(\.data), pastes: parent.pastes, skills: parent.skills)
        }

        init(_ parent: PromptEditor) { self.parent = parent }

        func attach(_ view: ChipTextView) {
            self.view = view
            view.coordinator = self
            view.delegate = self
            view.textStorage?.delegate = self
        }

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

        /// Shows the binding's text again if it, or what its chips stand for, changed. A chip goes
        /// in before the binding holds its image or paste, so it draws again once it does.
        func refresh(_ view: NSTextView) {
            if text(of: view) != parent.text || drawn != content {
                show(parent.text, in: view)
            }
        }

        /// Shows `text`, its markers as chips, keeping the caret where it was, or at the end if it
        /// was there (as when dictation adds to the text).
        func show(_ text: String, in view: NSTextView) {
            let shown = NSMutableAttributedString()
            var rest = text.startIndex
            for (range, token) in PromptText.tokens(in: text, skills: Set(parent.skills.map(\.name))) {
                shown.append(NSAttributedString(string: String(text[rest..<range.lowerBound]), attributes: PromptEditor.attributes))
                let dark = view.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                let attachment = ChipAttachment(token: token, label: ChipLabel(chip: chip(token)), dark: dark)
                let marker = NSMutableAttributedString(attachment: attachment)
                marker.addAttributes(PromptEditor.attributes, range: NSRange(location: 0, length: marker.length))
                shown.append(marker)
                rest = range.upperBound
            }
            shown.append(NSAttributedString(string: String(text[rest...]), attributes: PromptEditor.attributes))
            drawn = content
            let caret = view.selectedRange()
            let wasAtEnd = caret.location == 0 || caret.location >= (view.string as NSString).length
            showing = true
            view.textStorage?.setAttributedString(shown)
            showing = false
            let end = shown.length
            view.setSelectedRange(NSRange(location: wasAtEnd ? end : min(caret.location, end), length: 0))
        }

        /// Inserts text with markers at the caret, as typing would.
        func insert(_ text: String, in view: NSTextView) {
            guard !text.isEmpty else { return }
            view.insertText(text, replacementRange: view.selectedRange())
            parent.text = self.text(of: view)
            show(parent.text, in: view)
        }

        /// Every edit to the characters, typed or undone, goes to the binding. Undo does not
        /// send `textDidChange`, so the storage is what is watched.
        nonisolated func textStorage(_ storage: NSTextStorage, didProcessEditing mask: NSTextStorageEditActions,
                                     range: NSRange, changeInLength: Int) {
            guard mask.contains(.editedCharacters) else { return }
            MainActor.assumeIsolated {
                guard !showing, let view else { return }
                parent.text = text(of: view)
            }
        }

        func textDidBeginEditing(_ notification: Notification) { parent.focused = true }
        func textDidEndEditing(_ notification: Notification) { parent.focused = false }

        func textView(_ textView: NSTextView, doCommandBy selector: Selector) -> Bool {
            guard selector == #selector(NSResponder.insertNewline(_:)) else { return false }
            if NSApp?.currentEvent?.modifierFlags.contains(.shift) == true {
                newline(in: textView)
            } else {
                parent.submit()
            }
            return true
        }

        /// Shift-Enter: a new line at the caret; at the end, one that continues a list. It goes
        /// in as typing does, so the caret follows it and Undo takes it back.
        func newline(in textView: NSTextView) {
            let caret = textView.selectedRange()
            let length = (textView.string as NSString).length
            guard caret.location == length else {
                textView.insertText("\n", replacementRange: caret)
                return
            }
            let before = text(of: textView)
            let after = ListContinuation.newline(after: before)
            if after.hasPrefix(before) {
                textView.insertText(String(after.dropFirst(before.count)), replacementRange: caret)
            } else {
                // An empty item ends the list: its marker, plain text at the end, goes.
                let removed = before.utf16.count - after.utf16.count
                textView.insertText("", replacementRange: NSRange(location: length - removed, length: removed))
            }
        }

        func chip(_ token: PromptText.Token) -> PromptChip {
            PromptChip(token: token, image: token.kind == .image ? parent.images[safe: token.number - 1] : nil,
                 paste: token.kind == .paste ? parent.pastes[safe: token.number - 1] : nil,
                 skill: token.kind == .skill ? parent.skills.first(where: { $0.name == token.name }) : nil)
        }

        /// Opens what a chip stands for, under it.
        func open(_ token: PromptText.Token, at rect: NSRect, in view: NSView) {
            let popover = NSPopover()
            popover.behavior = .transient
            popover.contentViewController = NSHostingController(rootView: ChipDetail(chip: chip(token)) { [weak self, weak popover] in
                popover?.close()
                guard let self, let view = self.view else { return }
                self.parent.text = token.kind == .skill ? SkillMention.remove(token.name, from: self.parent.text)
                    : self.parent.text.replacingOccurrences(of: token.marker, with: "")
                self.show(self.parent.text, in: view)
            })
            popover.show(relativeTo: rect, of: view, preferredEdge: .maxY)
        }
    }
}

/// The text view: pastes and drops of images become chips, as do long pastes of text; other
/// files pasted or dropped are attached.
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
        if take(.general) {
            return
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
        take(sender.draggingPasteboard) || super.performDragOperation(sender)
    }

    /// Attaches a pasteboard's images, their markers at the caret, and its other files; whether
    /// it held any.
    private func take(_ board: NSPasteboard) -> Bool {
        guard let coordinator else { return false }
        let images = ImageAttachment.from(board)
        let urls = board.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
        let others = urls.filter { UTType(filenameExtension: $0.pathExtension)?.conforms(to: .image) != true }
        if !images.isEmpty { coordinator.insert(coordinator.parent.addImages(images), in: self) }
        if !others.isEmpty { coordinator.parent.addFiles(others) }
        return !images.isEmpty || !others.isEmpty
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
    var skill: SessionSkill?

    var name: String {
        switch token.kind {
        case .skill:
            return token.marker
        case .image:
            let ext = image.flatMap { $0.mediaType.split(separator: "/").last.map(String.init) } ?? "png"
            return token.number == 1 ? "image.\(ext)" : "image-\(token.number).\(ext)"
        case .paste:
            return token.number == 1 ? "pasted-text.txt" : "pasted-text-\(token.number).txt"
        }
    }

    var detail: String {
        switch token.kind {
        case .skill:
            // A chip in the text stays short; its popover has the whole description.
            let description = skill?.description ?? ""
            return description.count > 48 ? String(description.prefix(47)) + "…" : description
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
                SwiftUI.Image(systemName: chip.skill == nil ? "doc.text" : "book.closed").foregroundStyle(Theme.secondary)
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
            if let skill = chip.skill {
                Text(skill.description).foregroundStyle(Theme.text).frame(maxWidth: 420, alignment: .leading)
                    .fixedSize(horizontal: false, vertical: true)
                Text("\(skill.sourceLabel) skill\(skill.path.map { " · \($0)" } ?? "")")
                    .font(.caption).foregroundStyle(Theme.tertiary)
            } else if let image = chip.image {
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
#else
/// The composer's text view on iPhone and iPad: plain text, its markers ([`PromptText`]) as
/// typed, that takes pasted images and long pastes as the Mac's does. A SwiftUI text field
/// offers Paste only for text, so an image on the clipboard could not go in. It grows from two
/// lines to twelve, then scrolls.
struct PromptEditor: UIViewRepresentable {
    @Binding var text: String
    @Binding var focused: Bool
    /// Attaches pasted images; returns their markers, to go in at the caret.
    let addImages: ([Herder.Image]) -> String
    /// Takes long pasted text; returns its marker, to go in at the caret.
    let addPaste: (String) -> String
    /// Return on a hardware keyboard, without Shift.
    let submit: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> PasteTextView {
        let view = PasteTextView()
        view.coordinator = context.coordinator
        view.delegate = context.coordinator
        view.font = UIFont.preferredFont(forTextStyle: .body)
        view.adjustsFontForContentSizeCategory = true
        view.textColor = UIColor(Theme.text)
        view.backgroundColor = .clear
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.accessibilityIdentifier = "composer"
        view.text = text
        return view
    }

    func updateUIView(_ view: PasteTextView, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self
        if view.text != text { coordinator.show(text, in: view) }
        if focused, !view.isFirstResponder {
            DispatchQueue.main.async { view.becomeFirstResponder() }
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: PasteTextView, context: Context) -> CGSize? {
        guard let width = proposal.width, width.isFinite, width > 0 else { return nil }
        let line = (uiView.font ?? UIFont.preferredFont(forTextStyle: .body)).lineHeight
        let used = uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude)).height
        return CGSize(width: width, height: min(max(used, line * 2), line * 12).rounded(.up))
    }

    @MainActor final class Coordinator: NSObject, UITextViewDelegate {
        var parent: PromptEditor

        init(_ parent: PromptEditor) { self.parent = parent }

        /// Shows `text`, keeping the caret where it was, or at the end if it was there (as when
        /// dictation adds to the text).
        func show(_ text: String, in view: UITextView) {
            let caret = view.selectedRange
            let wasAtEnd = caret.location >= ((view.text ?? "") as NSString).length
            view.text = text
            let end = (text as NSString).length
            view.selectedRange = NSRange(location: wasAtEnd ? end : min(caret.location, end), length: 0)
        }

        /// Inserts text with markers at the caret, as typing would, so Undo takes it back.
        func insert(_ text: String, in view: UITextView) {
            guard !text.isEmpty else { return }
            view.insertText(text)
            parent.text = view.text ?? ""
        }

        func textViewDidChange(_ textView: UITextView) { parent.text = textView.text ?? "" }
        func textViewDidBeginEditing(_ textView: UITextView) { parent.focused = true }
        func textViewDidEndEditing(_ textView: UITextView) { parent.focused = false }

        /// Return: with a hardware keyboard it sends, and Shift-Return makes a new line; on the
        /// on-screen keyboard it makes a new line, the send button being right there. A new line
        /// at the end continues a list.
        func textView(_ textView: UITextView, shouldChangeTextIn range: NSRange, replacementText text: String) -> Bool {
            guard text == "\n", textView.markedTextRange == nil else { return true }
            if let keyboard = GCKeyboard.coalesced?.keyboardInput {
                let shift = keyboard.button(forKeyCode: .leftShift)?.isPressed == true
                    || keyboard.button(forKeyCode: .rightShift)?.isPressed == true
                if !shift {
                    parent.submit()
                    return false
                }
            }
            let before = textView.text ?? ""
            guard range.length == 0, range.location == (before as NSString).length else { return true }
            let after = ListContinuation.newline(after: before)
            if after == before + "\n" { return true }
            if after.hasPrefix(before) {
                textView.insertText(String(after.dropFirst(before.count)))
            } else if let start = textView.position(
                from: textView.endOfDocument, offset: -(before.utf16.count - after.utf16.count)),
                let tail = textView.textRange(from: start, to: textView.endOfDocument) {
                // An empty item ends the list: its marker, plain text at the end, goes.
                textView.replace(tail, withText: "")
            }
            parent.text = textView.text ?? ""
            return false
        }
    }
}

/// The text view: pasted images become `[Image #N]` markers, as do long pastes of text.
final class PasteTextView: UITextView {
    weak var coordinator: PromptEditor.Coordinator?

    /// A plain text view offers Paste only for text; images paste here too.
    override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
        if action == #selector(paste(_:)), isEditable, UIPasteboard.general.hasImages { return true }
        return super.canPerformAction(action, withSender: sender)
    }

    override func paste(_ sender: Any?) {
        guard let coordinator else { return super.paste(sender) }
        let board = UIPasteboard.general
        if board.hasImages {
            let images = ImageAttachment.from(board)
            if !images.isEmpty {
                coordinator.insert(coordinator.parent.addImages(images), in: self)
                return
            }
        }
        if board.hasStrings, let pasted = board.string, PromptText.isLong(pasted) {
            coordinator.insert(coordinator.parent.addPaste(pasted), in: self)
            return
        }
        super.paste(sender)
    }
}
#endif
