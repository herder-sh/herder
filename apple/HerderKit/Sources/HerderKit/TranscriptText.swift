import SwiftUI
#if os(iOS)
import UIKit
#endif

extension EnvironmentValues {
    /// The whole message a transcript text is part of, which its menu on a phone copies.
    @Entry var messageText: String?
}

/// Transcript text to select any part of and copy. A phone's SwiftUI text only copies all of
/// itself, from a menu, so there it is a text view, with the selection handles and the edit
/// menu of any other; that menu also copies the whole message.
struct TranscriptText: View {
    let string: AttributedString
    var style: Font.TextStyle = .body
    var monospaced = false
    var lineSpacing: CGFloat = 0
    var color: Color = Theme.text

    var body: some View {
        #if os(macOS)
        LinkText(string)
            .font(.system(style, design: monospaced ? .monospaced : .default))
            .foregroundStyle(color)
            .lineSpacing(lineSpacing)
            .textSelection(.enabled)
        #else
        SelectableTextView(text: Self.attributed(string, style: style, monospaced: monospaced,
                                                 lineSpacing: lineSpacing, color: UIColor(color)))
        #endif
    }
}

#if os(iOS)
extension TranscriptText {
    /// `string` in UIKit's attributes: its SwiftUI colours, the fonts `MarkdownText` gives
    /// headings, quotes and gaps, and Markdown's bold, italic, code and strikethrough.
    static func attributed(_ string: AttributedString, style: Font.TextStyle, monospaced: Bool,
                           lineSpacing: CGFloat, color: UIColor) -> NSAttributedString {
        let base = UIFont.preferredFont(forTextStyle: style.uiKit)
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineSpacing = lineSpacing
        let result = NSMutableAttributedString()
        for run in string.runs {
            var font = run.swiftUI.font.flatMap(uiFont) ?? (monospaced ? mono(base) : base)
            let intent = run.inlinePresentationIntent ?? []
            if intent.contains(.code) { font = mono(font) }
            var traits = font.fontDescriptor.symbolicTraits
            if intent.contains(.stronglyEmphasized) { traits.insert(.traitBold) }
            if intent.contains(.emphasized) { traits.insert(.traitItalic) }
            if let descriptor = font.fontDescriptor.withSymbolicTraits(traits) { font = UIFont(descriptor: descriptor, size: 0) }
            var attributes: [NSAttributedString.Key: Any] = [
                .font: font,
                .foregroundColor: run.swiftUI.foregroundColor.map(UIColor.init) ?? color,
                .paragraphStyle: paragraph,
            ]
            if let background = run.swiftUI.backgroundColor { attributes[.backgroundColor] = UIColor(background) }
            if let link = run.link { attributes[.link] = link }
            if intent.contains(.strikethrough) { attributes[.strikethroughStyle] = NSUnderlineStyle.single.rawValue }
            result.append(NSAttributedString(string: String(string[run.range].characters), attributes: attributes))
        }
        return result
    }

    private static func uiFont(_ font: Font) -> UIFont? {
        switch font {
        case MarkdownText.headingFont: .preferredFont(forTextStyle: .headline)
        case MarkdownText.quoteFont: UIFont.preferredFont(forTextStyle: .body).withItalic
        case MarkdownText.gapFont: .systemFont(ofSize: MarkdownText.gapSize)
        default: nil
        }
    }

    private static func mono(_ font: UIFont) -> UIFont { .monospacedSystemFont(ofSize: font.pointSize, weight: .regular) }
}

private extension UIFont {
    var withItalic: UIFont {
        fontDescriptor.withSymbolicTraits(.traitItalic).map { UIFont(descriptor: $0, size: 0) } ?? self
    }
}

private extension Font.TextStyle {
    var uiKit: UIFont.TextStyle {
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
        view.linkTextAttributes = [.foregroundColor: UIColor(Theme.link), .underlineStyle: NSUnderlineStyle.single.rawValue]
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
        let width = proposal.width.map { $0.isFinite ? $0 : .greatestFiniteMagnitude } ?? .greatestFiniteMagnitude
        let fit = text.boundingRect(with: CGSize(width: width, height: .greatestFiniteMagnitude),
                                    options: [.usesLineFragmentOrigin, .usesFontLeading], context: nil)
        return CGSize(width: min(width, ceil(fit.width)), height: ceil(fit.height))
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
#endif
