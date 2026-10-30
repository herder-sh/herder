import Herder
import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// An image a user message carries, as its `[Image #N]` marker stands for it: fetched from the
/// machine, or still on this device while the message is on its way.
struct MessagePicture: Hashable {
    let number: Int
    let mediaType: String
    let size: Int64
    /// The bytes, once they are here.
    let data: Data?
    /// Where to fetch the bytes from, when they are not here yet.
    let attachmentId: AttachmentId?

    var name: String {
        let ext = mediaType.split(separator: "/").last.map { $0 == "jpeg" ? "jpg" : String($0) } ?? "png"
        return number == 1 ? "image.\(ext)" : "image-\(number).\(ext)"
    }

    var detail: String { ByteCountFormatter.string(fromByteCount: size, countStyle: .file) }

    /// What stands for it in the message's text.
    var marker: String { "[Image #\(number)]" }

    /// A message's images, numbered in the order they were sent, as their markers are.
    @MainActor static func of(attachments: [Attachment], outgoing: Outgoing?, in fleet: Fleet) -> [MessagePicture] {
        if let outgoing {
            return outgoing.images.enumerated().map { index, image in
                MessagePicture(number: index + 1, mediaType: image.mediaType, size: Int64(image.data.count),
                               data: image.data, attachmentId: nil)
            }
        }
        return attachments.filter { !$0.isFile }.enumerated().map { index, attachment in
            MessagePicture(number: index + 1, mediaType: attachment.mediaType, size: Int64(attachment.size),
                           data: fleet.attachments[attachment.attachmentId], attachmentId: attachment.attachmentId)
        }
    }
}

/// A user message's text with each `[Image #N]` marker drawn as a chip of the image it stands
/// for, which opens the image on a click. One text view, so a drag selects across words and
/// chips alike, and a chip copies as its marker.
struct MessageText: View {
    let text: String
    let pictures: [MessagePicture]
    let fetch: (AttachmentId) async -> Void
    @Environment(\.prLinks) private var prLinks
    @Environment(\.findHighlight) private var find
    @State private var open: MessagePicture?

    var body: some View {
        let string = Self.marked(MarkdownText.inline(text, ground: Theme.onBubbleCode, prs: prLinks, find: find),
                                 pictures: pictures)
        TranscriptText(string: string, color: Theme.onBubble)
            .environment(\.openURL, OpenURLAction { url in
                guard let number = PictureChip.number(url) else { return .systemAction }
                open = pictures.first { $0.number == number }
                return .handled
            })
            .task(id: pictures.filter { $0.data == nil }.compactMap(\.attachmentId)) {
                for picture in pictures where picture.data == nil {
                    if let id = picture.attachmentId { await fetch(id) }
                }
            }
            .popover(isPresented: Binding(get: { open != nil }, set: { if !$0 { open = nil } })) {
                // The picture as it is now: its bytes may have come in since it was clicked.
                if let number = open?.number, let picture = pictures.first(where: { $0.number == number }) {
                    PicturePopover(picture: picture)
                }
            }
    }

    /// `string` with each marker that has a picture behind it marked as that picture's chip; a
    /// marker with no image behind it stays text.
    nonisolated static func marked(_ string: AttributedString, pictures: [MessagePicture]) -> AttributedString {
        var string = string
        let plain = String(string.characters)
        for (range, token) in PromptText.tokens(in: plain) where token.kind == .image {
            guard let picture = pictures.first(where: { $0.number == token.number }) else { continue }
            let start = string.characters.index(string.startIndex, offsetBy: plain.distance(from: plain.startIndex, to: range.lowerBound))
            let end = string.characters.index(start, offsetBy: plain.distance(from: range.lowerBound, to: range.upperBound))
            string[start..<end][PictureChip.self] = picture
        }
        return string
    }

    /// Whether the text has a marker for any of the pictures, so it draws as chips.
    nonisolated static func refers(_ text: String, to pictures: [MessagePicture]) -> Bool {
        PromptText.tokens(in: text).contains { token in
            token.token.kind == .image && pictures.contains { $0.number == token.token.number }
        }
    }
}

/// An image in a message's text, the picture its marker's characters stand for: drawn as a
/// thumbnail, the name and the size, as a link that shows it whole.
enum PictureChip: AttributedStringKey {
    typealias Value = MessagePicture
    static let name = "herder.message.picture"
    static let scheme = "herder-picture"

    static func url(_ picture: MessagePicture) -> URL? { URL(string: "\(scheme):\(picture.number)") }

    /// The number of the picture `url` shows; `nil` for any other link.
    static func number(_ url: URL) -> Int? {
        url.scheme == scheme ? Int(url.absoluteString.dropFirst(scheme.count + 1)) : nil
    }

    private struct Key: Hashable {
        let picture: MessagePicture
        let dark: Bool
        let scale: CGFloat
        let size: CGFloat
    }

    /// Drawn once for each look: a text view sets its text again only when it changed, and an
    /// attachment equals only itself.
    @MainActor private static var attachments: [Key: NSTextAttachment] = [:]

    /// The chip of `picture` as a text attachment, centred on the line of text in `font`.
    @MainActor static func attachment(_ picture: MessagePicture, dark: Bool, scale: CGFloat, font: PlatformFont) -> NSTextAttachment {
        let key = Key(picture: picture, dark: dark, scale: scale, size: font.pointSize)
        if let attachment = attachments[key] { return attachment }
        let renderer = ImageRenderer(content: Label(picture: picture)
            .environment(\.colorScheme, dark ? .dark : .light))
        renderer.scale = scale
        let attachment = NSTextAttachment()
        if let image = renderer.cgImage {
            let size = CGSize(width: CGFloat(image.width) / scale, height: CGFloat(image.height) / scale)
            #if os(macOS)
            let picture = NSImage(cgImage: image, size: size)
            attachment.image = picture
            attachment.attachmentCell = Cell(image: picture, baseline: ((font.capHeight - size.height) / 2).rounded())
            #else
            attachment.image = UIImage(cgImage: image, scale: scale, orientation: .up)
            #endif
            attachment.bounds = CGRect(x: 0, y: ((font.capHeight - size.height) / 2).rounded(), width: size.width, height: size.height)
        }
        // A message's pictures change as their bytes come in; the chips drawn before stay few.
        if attachments.count > 64 { attachments.removeAll() }
        attachments[key] = attachment
        return attachment
    }

    #if os(macOS)
    /// A chip as the Mac's text view draws it: centred on the line, and plain text to the mouse,
    /// so a drag from it selects rather than dragging the picture out.
    private final class Cell: NSTextAttachmentCell {
        private let baseline: CGFloat

        init(image: NSImage, baseline: CGFloat) {
            self.baseline = baseline
            super.init(imageCell: image)
        }

        required init(coder: NSCoder) {
            baseline = 0
            super.init(coder: coder)
        }

        override func cellBaselineOffset() -> NSPoint { NSPoint(x: 0, y: baseline) }
        override func wantsToTrackMouse() -> Bool { false }
    }
    #endif

    private struct Label: View {
        let picture: MessagePicture

        var body: some View {
            HStack(spacing: 5) {
                if let data = picture.data, let thumbnail = Thumbnail(data: data) {
                    thumbnail.image.resizable().scaledToFill()
                        .frame(width: 14, height: 14).clipShape(.rect(cornerRadius: 3))
                } else {
                    SwiftUI.Image(systemName: "photo").foregroundStyle(Theme.tertiary)
                }
                Text(picture.name).foregroundStyle(Theme.onBubble)
                Text(picture.detail).foregroundStyle(Theme.tertiary)
            }
            .font(.callout)
            .lineLimit(1)
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(Theme.raised, in: .rect(cornerRadius: 6))
            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(Theme.stroke))
            .fixedSize()
        }
    }
}

/// A picture of a message, whole, as a click on its chip shows it.
private struct PicturePopover: View {
    let picture: MessagePicture

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text(picture.name).font(.headline).foregroundStyle(Theme.text)
                Text(picture.detail).foregroundStyle(Theme.tertiary)
            }
            if let data = picture.data {
                Picture(data: data, height: 420)
            } else {
                ProgressView().tint(Theme.tertiary).frame(width: 320, height: 200)
            }
        }
        .padding(14)
        .frame(minWidth: 320)
        .presentationCompactAdaptation(.sheet)
    }
}

/// Image bytes as a SwiftUI image.
private struct Thumbnail {
    let image: SwiftUI.Image

    init?(data: Data) {
        #if os(macOS)
        guard let picture = NSImage(data: data) else { return nil }
        image = SwiftUI.Image(nsImage: picture)
        #else
        guard let picture = UIImage(data: data) else { return nil }
        image = SwiftUI.Image(uiImage: picture)
        #endif
    }
}
