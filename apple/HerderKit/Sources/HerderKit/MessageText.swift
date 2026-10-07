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
/// for, which opens the image on a click.
struct MessageText: View {
    let text: String
    let pictures: [MessagePicture]
    let fetch: (AttachmentId) async -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(Array(Self.lines(text, pictures: pictures).enumerated()), id: \.offset) { _, line in
                FlowLayout(spacing: 0, lineSpacing: 3) {
                    ForEach(Array(line.enumerated()), id: \.offset) { _, piece in
                        switch piece {
                        case .word(let word): Text(word)
                        case .picture(let picture): PictureChip(picture: picture, fetch: fetch)
                        }
                    }
                }
            }
        }
    }

    enum Piece: Hashable {
        case word(String), picture(MessagePicture)
    }

    /// The text's lines, each as words, with their spaces, and the chips between them. A marker
    /// with no image behind it stays text.
    nonisolated static func lines(_ text: String, pictures: [MessagePicture]) -> [[Piece]] {
        text.split(separator: "\n", omittingEmptySubsequences: false).map { line in
            let line = String(line)
            var pieces: [Piece] = []
            var rest = line.startIndex
            // Each word with the spaces after it, so the line wraps between words.
            func words(_ part: Substring) {
                var word = ""
                for character in part {
                    if !character.isWhitespace, word.last?.isWhitespace == true {
                        pieces.append(.word(word))
                        word = ""
                    }
                    word.append(character)
                }
                if !word.isEmpty { pieces.append(.word(word)) }
            }
            for (range, token) in PromptText.tokens(in: line) where token.kind == .image {
                guard let picture = pictures.first(where: { $0.number == token.number }) else { continue }
                words(line[rest..<range.lowerBound])
                pieces.append(.picture(picture))
                rest = range.upperBound
            }
            words(line[rest...])
            return pieces.isEmpty ? [.word(" ")] : pieces
        }
    }

    /// Whether the text has a marker for any of the pictures, so it draws as chips.
    nonisolated static func refers(_ text: String, to pictures: [MessagePicture]) -> Bool {
        PromptText.tokens(in: text).contains { token in
            token.token.kind == .image && pictures.contains { $0.number == token.token.number }
        }
    }
}

/// An image in a message's text: a thumbnail, the name and the size; a click shows it whole.
private struct PictureChip: View {
    let picture: MessagePicture
    let fetch: (AttachmentId) async -> Void
    @State private var open = false

    var body: some View {
        Button { open = true } label: {
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
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .task { if picture.data == nil, let id = picture.attachmentId { await fetch(id) } }
        .popover(isPresented: $open) {
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
