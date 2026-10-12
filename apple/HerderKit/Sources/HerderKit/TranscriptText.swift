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

/// Transcript text to select any part of and copy, in a text view on both platforms: SwiftUI's
/// text has no hanging indent for list items nor space between paragraphs, and on a phone only
/// copies all of itself. Its menu also copies the whole message.
struct TranscriptText: View {
    let string: AttributedString
    var style: Font.TextStyle = .body
    var monospaced = false
    var lineSpacing: CGFloat = 0
    var color: Color = Theme.text

    var body: some View {
        SelectableTextView(text: Self.attributed(string, style: style, monospaced: monospaced,
                                                 lineSpacing: lineSpacing, color: PlatformColor(color)))
    }
}

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
    /// Links keep the text's colour, underlined in a quieter one.
    static func attributed(_ string: AttributedString, style: Font.TextStyle, monospaced: Bool,
                           lineSpacing: CGFloat, color: PlatformColor) -> NSAttributedString {
        let base = PlatformFont.preferredFont(forTextStyle: style.platform)
        let text = monospaced ? PlatformFont.monospacedSystemFont(ofSize: base.pointSize, weight: .regular) : base
        // One indent column fits a list marker up to "99.".
        let column = (base.pointSize * 1.6).rounded()
        var paragraphs: [MarkdownText.Paragraph: NSParagraphStyle] = [:]
        let result = NSMutableAttributedString()
        for run in string.runs {
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
                paragraph.paragraphSpacingBefore = layout.spaceBefore
                paragraph.headIndent = CGFloat(layout.indent) * column
                paragraph.firstLineHeadIndent = paragraph.headIndent - (layout.marker ? column : 0)
                paragraph.tabStops = [NSTextTab(textAlignment: .left, location: paragraph.headIndent)]
                paragraph.defaultTabInterval = column
                paragraphs[layout] = paragraph
                return paragraph
            }()
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
            }
            if intent.contains(.strikethrough) { attributes[.strikethroughStyle] = NSUnderlineStyle.single.rawValue }
            result.append(NSAttributedString(string: String(string[run.range].characters), attributes: attributes))
        }
        return result
    }
}

private extension PlatformFont {
    func adding(bold: Bool = false, italic: Bool = false) -> PlatformFont {
        guard bold || italic else { return self }
        #if os(iOS)
        var traits = fontDescriptor.symbolicTraits
        if bold { traits.insert(.traitBold) }
        if italic { traits.insert(.traitItalic) }
        return fontDescriptor.withSymbolicTraits(traits).map { UIFont(descriptor: $0, size: 0) } ?? self
        #else
        var traits = fontDescriptor.symbolicTraits
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

/// The size of `text` laid out in `proposal`'s width.
@MainActor private func fit(_ text: NSAttributedString, in proposal: ProposedViewSize) -> CGSize {
    let width = proposal.width.map { $0.isFinite ? $0 : .greatestFiniteMagnitude } ?? .greatestFiniteMagnitude
    let fit = text.boundingRect(with: CGSize(width: width, height: .greatestFiniteMagnitude),
                                options: [.usesLineFragmentOrigin, .usesFontLeading], context: nil)
    return CGSize(width: min(width, ceil(fit.width)), height: ceil(fit.height))
}

#if os(iOS)
/// A text view that only selects: sized to its text, laid out by SwiftUI, links opened by the
/// environment's `openURL`.
private struct SelectableTextView: UIViewRepresentable {
    let text: NSAttributedString

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> UITextView {
        let view = UITextView(usingTextLayoutManager: true)
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
        context.coordinator.openURL = context.environment.openURL
        context.coordinator.message = context.environment.messageText
        // Setting the same text again would drop a selection, as a streaming message redraws.
        if context.coordinator.text != text {
            context.coordinator.text = text
            view.attributedText = text
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UITextView, context: Context) -> CGSize? {
        fit(text, in: proposal)
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
            guard case .link(let url) = textItem.content, let openURL else { return defaultAction }
            return UIAction { _ in openURL(url) }
        }
    }
}
#else
/// A text view that only selects: sized to its text, laid out by SwiftUI, links opened by the
/// environment's `openURL`, with the pointing hand over them.
private struct SelectableTextView: NSViewRepresentable {
    let text: NSAttributedString

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSTextView {
        let view = NSTextView(usingTextLayoutManager: true)
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
        context.coordinator.openURL = context.environment.openURL
        context.coordinator.message = context.environment.messageText
        // Setting the same text again would drop a selection, as a streaming message redraws.
        if context.coordinator.text != text {
            context.coordinator.text = text
            view.textStorage?.setAttributedString(text)
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: NSTextView, context: Context) -> CGSize? {
        fit(text, in: proposal)
    }

    @MainActor final class Coordinator: NSObject, NSTextViewDelegate {
        var text: NSAttributedString?
        var message: String?
        var openURL: OpenURLAction?

        func textView(_ textView: NSTextView, clickedOnLink link: Any, at charIndex: Int) -> Bool {
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
#endif
