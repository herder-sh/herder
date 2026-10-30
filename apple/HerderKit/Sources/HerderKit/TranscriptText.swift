import SwiftUI
#if os(iOS)
import UIKit
#else
import AppKit
#endif

extension EnvironmentValues {
    /// The whole message a transcript text is part of, which its menu copies.
    @Entry var messageText: String?
}

/// Text to select any part of and copy, in a text view: the transcript's, with the hanging
/// indents and paragraph spacing of `MarkdownText`'s prose, and any other text worth copying; its menu also copies the whole message, and a
/// click or tap on inline code copies just that span. On a Mac,
/// SwiftUI's selectable text hands its selection to Copy only on its next update, so a ⌘C right
/// after selecting copied nothing; on a phone, SwiftUI's text only copies all of itself.
struct TranscriptText: View {
    let string: AttributedString
    var style: Font.TextStyle = .body
    var weight: PlatformFont.Weight = .regular
    var monospaced = false
    var italic = false
    var lineSpacing: CGFloat = 0
    var color: Color = Theme.text
    var alignment: NSTextAlignment = .natural
    /// The most lines it shows, the last cut short; all of them if `nil`.
    var lineLimit: Int?
    @Environment(\.colorScheme) private var scheme
    @Environment(\.displayScale) private var scale

    var body: some View {
        let text = Self.attributed(string, style: style, weight: weight, monospaced: monospaced, italic: italic,
                                   lineSpacing: lineSpacing, color: PlatformColor(color), alignment: alignment,
                                   dark: scheme == .dark, scale: scale)
        let font = text.length > 0 ? text.attribute(.font, at: 0, effectiveRange: nil) as? PlatformFont : nil
        SelectableTextView(text: text, lineLimit: lineLimit)
            // On the line of the text beside it, as a `Text` would be.
            .alignmentGuide(.firstTextBaseline) { _ in font?.ascender ?? 0 }
    }
}

extension TranscriptText {
    /// Plain `text`, to select and copy.
    init(_ text: String, style: Font.TextStyle = .body, weight: PlatformFont.Weight = .regular, monospaced: Bool = false,
         italic: Bool = false, color: Color = Theme.text, lineLimit: Int? = nil) {
        self.init(string: AttributedString(text), style: style, weight: weight, monospaced: monospaced, italic: italic,
                  color: color, lineLimit: lineLimit)
    }
}

/// Inline code as a link that copies it rather than opening anything: a click (or tap) on a span
/// copies exactly its text, while a drag still selects. Its own scheme, which `openURL` and the
/// `herder://` links never see.
enum CodeCopy {
    static let scheme = "herder-copy"

    static func url(_ code: String) -> URL? {
        code.addingPercentEncoding(withAllowedCharacters: .alphanumerics).flatMap { URL(string: "\(scheme):\($0)") }
    }

    /// The code `url` copies; `nil` for any other link.
    static func code(_ url: URL) -> String? {
        let prefix = scheme + ":"
        guard url.absoluteString.hasPrefix(prefix) else { return nil }
        return url.absoluteString.dropFirst(prefix.count).removingPercentEncoding
    }
}

extension NSAttributedString.Key {
    /// What a character copies as in place of itself: a picture chip, its marker.
    static let copiedAs = NSAttributedString.Key("herder.copiedAs")
    /// What a click on a picture chip opens. On a Mac not a link: a drag from a link selects
    /// only the link, and a drag across a prompt starts on its chip.
    static let chipLink = NSAttributedString.Key("herder.chipLink")
}

extension NSAttributedString {
    /// The text `range` copies as: its characters, a chip as what it stands for.
    func copied(_ range: NSRange) -> String {
        var text = ""
        enumerateAttribute(.copiedAs, in: range) { value, range, _ in
            text += value as? String ?? attributedSubstring(from: range).string
        }
        return text
    }

    /// The code span at `index` and the code it copies.
    func codeSpan(at index: Int) -> (range: NSRange, code: String)? {
        guard index >= 0, index < length else { return nil }
        var range = NSRange()
        guard let url = attribute(.link, at: index, longestEffectiveRange: &range, in: NSRange(location: 0, length: length)) as? URL,
              let code = CodeCopy.code(url) else { return nil }
        return (range, code)
    }
}

/// What shows over a code span once a click copied it.
private struct CopiedBadge: View {
    static let id = "copied-badge"

    var body: some View {
        Label("Copied", systemImage: "checkmark")
            .font(.caption.weight(.semibold))
            .foregroundStyle(Theme.canvas)
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(Theme.text, in: .capsule)
            .fixedSize()
    }
}

/// How long "Copied" stays over a span.
private let copiedFor = Duration.seconds(1.2)

#if os(iOS)
typealias PlatformFont = UIFont
typealias PlatformColor = UIColor
#else
typealias PlatformFont = NSFont
typealias PlatformColor = NSColor
#endif

extension TranscriptText {
    /// Inline code against the text around it.
    static let codeScale: CGFloat = 0.88

    /// `string` in the text views' attributes: its SwiftUI colours, the fonts and paragraphs
    /// `MarkdownText` gives its prose, and Markdown's bold, italic, code and strikethrough.
    /// Links keep the text's colour, underlined in a quieter one. A picture's marker becomes its
    /// chip, drawn for the `dark` or light look at `scale`, that copies as the marker.
    static func attributed(_ string: AttributedString, style: Font.TextStyle, weight: PlatformFont.Weight = .regular,
                           monospaced: Bool, italic: Bool = false, lineSpacing: CGFloat, color: PlatformColor,
                           alignment: NSTextAlignment = .natural, dark: Bool = false, scale: CGFloat = 2) -> NSAttributedString {
        let base = PlatformFont.preferredFont(forTextStyle: style.platform)
        let text = (monospaced ? PlatformFont.monospacedSystemFont(ofSize: base.pointSize, weight: weight)
                    : weight == .regular ? base : PlatformFont.systemFont(ofSize: base.pointSize, weight: weight))
            .adding(italic: italic)
        // One indent column fits a list marker up to "99.".
        let column = (base.pointSize * 1.6).rounded()
        var paragraphs: [MarkdownText.Paragraph: NSParagraphStyle] = [:]
        let spans = codeSpans(string)
        var span = spans.startIndex
        let result = NSMutableAttributedString()
        // The picture of the run before, if any: find's marks may split a marker into several runs.
        var previous: MessagePicture?
        for run in string.runs {
            defer { previous = run[PictureChip.self] }
            var font = switch run[MarkdownText.Role.self] {
            case .heading: PlatformFont.preferredFont(forTextStyle: .headline)
            case .label: PlatformFont.systemFont(ofSize: PlatformFont.preferredFont(forTextStyle: .caption1).pointSize, weight: .semibold)
            case .marker: PlatformFont.monospacedDigitSystemFont(ofSize: base.pointSize, weight: .regular)
            case .quote: text.adding(italic: true)
            case nil: text
            }
            let intent = run.inlinePresentationIntent ?? []
            if intent.contains(.code) {
                font = .monospacedSystemFont(ofSize: (font.pointSize * codeScale).rounded(), weight: .regular)
            }
            font = font.adding(bold: intent.contains(.stronglyEmphasized), italic: intent.contains(.emphasized))
            let layout = run[MarkdownText.Paragraph.self] ?? MarkdownText.Paragraph()
            let paragraph = paragraphs[layout] ?? {
                let paragraph = NSMutableParagraphStyle()
                paragraph.lineSpacing = lineSpacing
                paragraph.alignment = alignment
                paragraph.paragraphSpacingBefore = layout.spaceBefore
                paragraph.headIndent = CGFloat(layout.indent) * column
                paragraph.firstLineHeadIndent = paragraph.headIndent - (layout.marker ? column : 0)
                paragraph.tabStops = [NSTextTab(textAlignment: .left, location: paragraph.headIndent)]
                paragraph.defaultTabInterval = column
                paragraphs[layout] = paragraph
                return paragraph
            }()
            if let picture = run[PictureChip.self] {
                guard picture != previous else { continue }
                let chip = NSMutableAttributedString(attachment: PictureChip.attachment(picture, dark: dark, scale: scale, font: font))
                chip.addAttributes([.font: font, .paragraphStyle: paragraph, .copiedAs: picture.marker],
                                   range: NSRange(location: 0, length: chip.length))
                if let url = PictureChip.url(picture) {
                    #if os(iOS)
                    chip.addAttribute(.link, value: url, range: NSRange(location: 0, length: chip.length))
                    #else
                    chip.addAttribute(.chipLink, value: url, range: NSRange(location: 0, length: chip.length))
                    #endif
                }
                result.append(chip)
                continue
            }
            var attributes: [NSAttributedString.Key: Any] = [
                .font: font,
                .foregroundColor: run.swiftUI.foregroundColor.map(PlatformColor.init) ?? color,
                .paragraphStyle: paragraph,
            ]
            if run[MarkdownText.Role.self] == .label { attributes[.kern] = 0.6 }
            if let background = run.swiftUI.backgroundColor { attributes[.backgroundColor] = PlatformColor(background) }
            if let link = run.link {
                attributes[.link] = link
                attributes[.underlineStyle] = NSUnderlineStyle.single.rawValue
                attributes[.underlineColor] = PlatformColor(Theme.tertiary)
            } else if intent.contains(.code) {
                while span < spans.endIndex, spans[span].range.upperBound <= run.range.lowerBound { span += 1 }
                if span < spans.endIndex { attributes[.link] = spans[span].url }
            }
            if intent.contains(.strikethrough) { attributes[.strikethroughStyle] = NSUnderlineStyle.single.rawValue }
            result.append(NSAttributedString(string: String(string[run.range].characters), attributes: attributes))
        }
        return result
    }

    /// `range`, a drag's selection, grown by `pressed`, the character the drag started on, if
    /// it stops just short of it: pressed past a letter's middle, the drag anchors after it.
    static func dragSelection(_ range: NSRange, keeping pressed: Int) -> NSRange {
        guard range.length > 0 else { return range }
        if pressed == range.location - 1 { return NSRange(location: pressed, length: range.length + 1) }
        if pressed == NSMaxRange(range) { return NSRange(location: range.location, length: range.length + 1) }
        return range
    }

    /// Each inline code span of `string` that is not a link, as one range however many runs
    /// find's marks split it into, with the link that copies its text.
    static func codeSpans(_ string: AttributedString) -> [(range: Range<AttributedString.Index>, url: URL)] {
        var ranges: [Range<AttributedString.Index>] = []
        for run in string.runs where run.inlinePresentationIntent?.contains(.code) == true && run.link == nil {
            if let last = ranges.last, last.upperBound == run.range.lowerBound {
                ranges[ranges.count - 1] = last.lowerBound..<run.range.upperBound
            } else {
                ranges.append(run.range)
            }
        }
        return ranges.compactMap { range in
            CodeCopy.url(String(string[range].characters)).map { (range, $0) }
        }
    }
}

private extension PlatformFont {
    func adding(bold: Bool = false, italic: Bool = false) -> PlatformFont {
        guard bold || italic else { return self }
        var traits = fontDescriptor.symbolicTraits
        #if os(iOS)
        if bold { traits.insert(.traitBold) }
        if italic { traits.insert(.traitItalic) }
        return fontDescriptor.withSymbolicTraits(traits).map { UIFont(descriptor: $0, size: 0) } ?? self
        #else
        if bold { traits.insert(.bold) }
        if italic { traits.insert(.italic) }
        return NSFont(descriptor: fontDescriptor.withSymbolicTraits(traits), size: 0) ?? self
        #endif
    }
}

private extension Font.TextStyle {
    var platform: PlatformFont.TextStyle {
        switch self {
        case .largeTitle: .largeTitle
        case .title: .title1
        case .title2: .title2
        case .title3: .title3
        case .headline: .headline
        case .subheadline: .subheadline
        case .callout: .callout
        case .footnote: .footnote
        case .caption: .caption1
        case .caption2: .caption2
        default: .body
        }
    }
}

/// The size of `text` laid out in `proposal`'s width, in at most `lineLimit` lines.
@MainActor private func fit(_ text: NSAttributedString, in proposal: ProposedViewSize, lineLimit: Int?) -> CGSize {
    let width = proposal.width.map { $0.isFinite ? $0 : .greatestFiniteMagnitude } ?? .greatestFiniteMagnitude
    let fit = text.boundingRect(with: CGSize(width: width, height: .greatestFiniteMagnitude),
                                options: [.usesLineFragmentOrigin, .usesFontLeading], context: nil)
    var height = ceil(fit.height)
    if let lineLimit, text.length > 0, let font = text.attribute(.font, at: 0, effectiveRange: nil) as? PlatformFont {
        let spacing = (text.attribute(.paragraphStyle, at: 0, effectiveRange: nil) as? NSParagraphStyle)?.lineSpacing ?? 0
        let line = font.ascender - font.descender + font.leading
        height = min(height, ceil(CGFloat(lineLimit) * line + CGFloat(lineLimit - 1) * spacing))
    }
    return CGSize(width: min(width, ceil(fit.width)), height: height)
}

#if os(iOS)
/// A text view that only selects: sized to its text, laid out by SwiftUI, links opened by the
/// environment's `openURL`.
private struct SelectableTextView: UIViewRepresentable {
    let text: NSAttributedString
    let lineLimit: Int?

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> UITextView {
        let view = ChipTextView(usingTextLayoutManager: true)
        view.isEditable = false
        view.isScrollEnabled = false
        view.backgroundColor = .clear
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        // The text's own attributes style its links.
        view.linkTextAttributes = [:]
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.delegate = context.coordinator
        return view
    }

    func updateUIView(_ view: UITextView, context: Context) {
        view.textContainer.maximumNumberOfLines = lineLimit ?? 0
        view.textContainer.lineBreakMode = lineLimit == nil ? .byWordWrapping : .byTruncatingTail
        context.coordinator.openURL = context.environment.openURL
        context.coordinator.message = context.environment.messageText
        // Setting the same text again would drop a selection, as a streaming message redraws.
        if context.coordinator.text != text {
            context.coordinator.text = text
            view.attributedText = text
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UITextView, context: Context) -> CGSize? {
        fit(text, in: proposal, lineLimit: lineLimit)
    }

    @MainActor final class Coordinator: NSObject, UITextViewDelegate {
        var text: NSAttributedString?
        var message: String?
        var openURL: OpenURLAction?

        func textView(_ textView: UITextView, editMenuForTextIn range: NSRange,
                      suggestedActions: [UIMenuElement]) -> UIMenu? {
            guard let message else { return nil }
            let copy = UIAction(title: "Copy Message", image: UIImage(systemName: "doc.on.doc")) { _ in
                Clipboard.string = message
            }
            // Next to Copy, ahead of Look Up and the rest, which a narrow menu pages away.
            var children = suggestedActions
            let edit = children.firstIndex { ($0 as? UIMenu)?.identifier == .standardEdit }
            children.insert(copy, at: edit.map { $0 + 1 } ?? children.endIndex)
            return UIMenu(children: children)
        }

        func textView(_ textView: UITextView, primaryActionFor textItem: UITextItem,
                      defaultAction: UIAction) -> UIAction? {
            // A picture chip is an attachment under a link.
            let link: URL? = switch textItem.content {
            case .link(let url): url
            case .textAttachment: textView.attributedText.attribute(.link, at: textItem.range.location, effectiveRange: nil) as? URL
            default: nil
            }
            guard let url = link else { return defaultAction }
            if let code = CodeCopy.code(url) {
                return UIAction { [weak textView] _ in
                    Clipboard.string = code
                    textView?.showCopied(over: textItem.range)
                }
            }
            guard let openURL else { return defaultAction }
            return UIAction { _ in openURL(url) }
        }

        /// No link menu over a code span: a long press there selects, as anywhere in the message.
        func textView(_ textView: UITextView, menuConfigurationFor textItem: UITextItem,
                      defaultMenu: UIMenu) -> UITextItem.MenuConfiguration? {
            if case .link(let url) = textItem.content, CodeCopy.code(url) != nil { return nil }
            return UITextItem.MenuConfiguration(preview: .default, menu: defaultMenu)
        }
    }
}

/// A text view whose edit menu selects all of the message, as a read-only text view's does not,
/// and whose picture chips copy as their markers.
private final class ChipTextView: UITextView {
    override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
        if action == #selector(selectAll(_:)) { return selectedRange.length < attributedText.length }
        return super.canPerformAction(action, withSender: sender)
    }

    override func copy(_ sender: Any?) {
        let range = selectedRange
        guard range.length > 0, attributedText.containsAttachments(in: range) else { return super.copy(sender) }
        Clipboard.string = attributedText.copied(range)
    }
}

private extension UITextView {
    /// "Copied" over the first line of `range`, for a moment.
    func showCopied(over range: NSRange) {
        guard let start = position(from: beginningOfDocument, offset: range.location),
              let end = position(from: start, offset: range.length),
              let span = textRange(from: start, to: end) else { return }
        let rect = firstRect(for: span)
        subviews.filter { $0.accessibilityIdentifier == CopiedBadge.id }.forEach { $0.removeFromSuperview() }
        let badge: UIView = UIHostingController(rootView: CopiedBadge()).view
        badge.accessibilityIdentifier = CopiedBadge.id
        badge.backgroundColor = .clear
        let size = badge.sizeThatFits(UIView.layoutFittingCompressedSize)
        badge.frame = CGRect(origin: CGPoint(x: rect.midX - size.width / 2, y: rect.midY - size.height / 2), size: size)
        addSubview(badge)
        Task { @MainActor [weak badge] in
            try? await Task.sleep(for: copiedFor)
            UIView.animate(withDuration: 0.2, animations: { badge?.alpha = 0 }, completion: { _ in badge?.removeFromSuperview() })
        }
    }
}
#else
/// A text view that only selects: sized to its text, laid out by SwiftUI, links opened by the
/// environment's `openURL`, with the pointing hand over them.
private struct SelectableTextView: NSViewRepresentable {
    let text: NSAttributedString
    let lineLimit: Int?

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSTextView {
        // TextKit 1, which lays out all of the text. TextKit 2 lays out only the part of it it
        // takes to be in view, and a transcript opens at its end: a tall reply drew blank.
        let view = CodeCopyTextView(usingTextLayoutManager: false)
        view.isEditable = false
        view.isSelectable = true
        view.drawsBackground = false
        view.isVerticallyResizable = false
        view.isHorizontallyResizable = false
        view.textContainerInset = .zero
        view.textContainer?.lineFragmentPadding = 0
        view.textContainer?.widthTracksTextView = true
        // The text's own attributes style its links.
        view.linkTextAttributes = [.cursor: NSCursor.pointingHand]
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.delegate = context.coordinator
        return view
    }

    func updateNSView(_ view: NSTextView, context: Context) {
        view.textContainer?.maximumNumberOfLines = lineLimit ?? 0
        view.textContainer?.lineBreakMode = lineLimit == nil ? .byWordWrapping : .byTruncatingTail
        context.coordinator.openURL = context.environment.openURL
        context.coordinator.message = context.environment.messageText
        // Setting the same text again would drop a selection, as a streaming message redraws.
        if context.coordinator.text != text {
            context.coordinator.text = text
            view.textStorage?.setAttributedString(text)
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: NSTextView, context: Context) -> CGSize? {
        fit(text, in: proposal, lineLimit: lineLimit)
    }

    @MainActor final class Coordinator: NSObject, NSTextViewDelegate {
        var text: NSAttributedString?
        var message: String?
        var openURL: OpenURLAction?

        func textView(_ textView: NSTextView, clickedOnLink link: Any, at charIndex: Int) -> Bool {
            if let span = textView.textStorage?.codeSpan(at: charIndex) {
                Clipboard.string = span.code
                textView.showCopied(over: span.range)
                return true
            }
            guard let url = link as? URL ?? (link as? String).flatMap(URL.init(string:)), let openURL else { return false }
            openURL(url)
            return true
        }

        func textView(_ view: NSTextView, menu: NSMenu, for event: NSEvent, at charIndex: Int) -> NSMenu? {
            guard let message else { return menu }
            let copy = NSMenuItem(title: "Copy Message", action: #selector(copyMessage), keyEquivalent: "")
            copy.target = self
            copy.representedObject = message
            // Next to Copy, ahead of Look Up and the rest.
            let after = menu.items.firstIndex { $0.action == #selector(NSText.copy(_:)) }
            menu.insertItem(copy, at: after.map { $0 + 1 } ?? 0)
            return menu
        }

        @objc private func copyMessage(_ item: NSMenuItem) {
            if let message = item.representedObject as? String { Clipboard.string = message }
        }
    }
}

/// A text view that lights up the code span under the pointer, as a hint that a click copies it,
/// and whose drag selects the letter it starts on. NSTextView anchors a drag at the gap nearest
/// the press, so one pressed on the right half of a letter left that letter out. Its picture
/// chips copy as their markers.
private final class CodeCopyTextView: NSTextView {
    private var hovered: NSRange?
    /// The character a plain press landed on, while its drag selects.
    private var pressed: Int?
    private lazy var tracking = NSTrackingArea(rect: .zero, options: [.mouseMoved, .mouseEnteredAndExited,
                                                                       .activeInKeyWindow, .inVisibleRect], owner: self)

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if !trackingAreas.contains(tracking) { addTrackingArea(tracking) }
    }

    override func mouseMoved(with event: NSEvent) {
        super.mouseMoved(with: event)
        highlight(span(at: convert(event.locationInWindow, from: nil))?.range)
    }

    override func mouseExited(with event: NSEvent) {
        super.mouseExited(with: event)
        highlight(nil)
    }

    override func mouseDown(with event: NSEvent) {
        let plain = event.clickCount == 1 && event.modifierFlags.isDisjoint(with: [.shift, .command, .option])
        pressed = plain ? character(at: convert(event.locationInWindow, from: nil)) : nil
        defer { pressed = nil }
        // A press on a picture chip opens it if it lets go there, and selects if it drags on.
        if let pressed, let url = textStorage?.attribute(.chipLink, at: pressed, effectiveRange: nil) as? URL,
           let next = window?.nextEvent(matching: [.leftMouseUp, .leftMouseDragged]) {
            if next.type == .leftMouseUp {
                _ = delegate?.textView?(self, clickedOnLink: url, at: pressed)
                return
            }
            window?.postEvent(next, atStart: true)
        }
        super.mouseDown(with: event)
    }

    override func copy(_ sender: Any?) {
        let range = selectedRange()
        guard let textStorage, selectedRanges.count == 1, range.length > 0,
              textStorage.containsAttachments(in: range) else { return super.copy(sender) }
        Clipboard.string = textStorage.copied(range)
    }

    override func setSelectedRanges(_ ranges: [NSValue], affinity: NSSelectionAffinity, stillSelecting: Bool) {
        guard let pressed, ranges.count == 1 else {
            return super.setSelectedRanges(ranges, affinity: affinity, stillSelecting: stillSelecting)
        }
        super.setSelectedRanges([NSValue(range: TranscriptText.dragSelection(ranges[0].rangeValue, keeping: pressed))],
                                affinity: affinity, stillSelecting: stillSelecting)
    }

    /// The character drawn at `point`; none between lines or past their ends.
    private func character(at point: NSPoint) -> Int? {
        guard let layoutManager, let textContainer else { return nil }
        let location = NSPoint(x: point.x - textContainerOrigin.x, y: point.y - textContainerOrigin.y)
        let glyph = layoutManager.glyphIndex(for: location, in: textContainer)
        guard layoutManager.boundingRect(forGlyphRange: NSRange(location: glyph, length: 1), in: textContainer)
            .contains(location) else { return nil }
        return layoutManager.characterIndexForGlyph(at: glyph)
    }

    private func span(at point: NSPoint) -> (range: NSRange, code: String)? {
        character(at: point).flatMap { textStorage?.codeSpan(at: $0) }
    }

    private func highlight(_ range: NSRange?) {
        guard range != hovered, let layoutManager else { return }
        // The text may have changed under it since, as a message streams.
        let whole = NSRange(location: 0, length: textStorage?.length ?? 0)
        if let hovered { layoutManager.removeTemporaryAttribute(.backgroundColor, forCharacterRange: NSIntersectionRange(hovered, whole)) }
        if let range { layoutManager.addTemporaryAttribute(.backgroundColor, value: NSColor(Theme.stroke), forCharacterRange: range) }
        hovered = range
    }
}

private extension NSTextView {
    /// "Copied" over the first line of `range`, for a moment.
    func showCopied(over range: NSRange) {
        guard let layoutManager, let textContainer else { return }
        let glyphs = layoutManager.glyphRange(forCharacterRange: range, actualCharacterRange: nil)
        var line = CGRect.null
        layoutManager.enumerateEnclosingRects(forGlyphRange: glyphs, withinSelectedGlyphRange: NSRange(location: NSNotFound, length: 0),
                                              in: textContainer) { rect, stop in
            line = rect
            stop.pointee = true
        }
        guard !line.isNull else { return }
        let rect = line.offsetBy(dx: textContainerOrigin.x, dy: textContainerOrigin.y)
        subviews.filter { $0.identifier?.rawValue == CopiedBadge.id }.forEach { $0.removeFromSuperview() }
        let badge = NSHostingView(rootView: CopiedBadge())
        badge.identifier = NSUserInterfaceItemIdentifier(CopiedBadge.id)
        let size = badge.fittingSize
        badge.frame = CGRect(origin: CGPoint(x: rect.midX - size.width / 2, y: rect.midY - size.height / 2), size: size)
        addSubview(badge)
        Task { @MainActor [weak badge] in
            try? await Task.sleep(for: copiedFor)
            NSAnimationContext.runAnimationGroup { $0.duration = 0.2; badge?.animator().alphaValue = 0 } completionHandler: {
                badge?.removeFromSuperview()
            }
        }
    }
}
#endif
