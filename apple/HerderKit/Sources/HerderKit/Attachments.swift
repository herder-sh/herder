import Herder
import SwiftUI
import UniformTypeIdentifiers
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// Images for a prompt: in a type and size the daemon takes (`imageMediaTypes()`,
/// `maxImageBytes()`), converted to JPEG and scaled down when they are not.
enum ImageAttachment {
    struct Refused: LocalizedError {
        let errorDescription: String?
    }

    /// An image from its bytes, as the clipboard, a drop or a file gives them.
    static func make(_ data: Data, type: UTType?) throws -> Herder.Image {
        let limit = Int(maxImageBytes())
        if let mediaType = type?.preferredMIMEType, imageMediaTypes().contains(mediaType), data.count <= limit {
            return Herder.Image(mediaType: mediaType, data: data)
        }
        guard let jpeg = jpeg(data, limit: limit) else {
            throw Refused(errorDescription: "That image cannot be sent: it is not a picture, or it is too large.")
        }
        return Herder.Image(mediaType: "image/jpeg", data: jpeg)
    }

    /// The image as JPEG within `limit` bytes, scaling it down until it fits.
    private static func jpeg(_ data: Data, limit: Int) -> Data? {
        #if os(macOS)
        guard let image = NSImage(data: data), let tiff = image.tiffRepresentation,
              var rep = NSBitmapImageRep(data: tiff) else { return nil }
        for _ in 0..<4 {
            if let out = rep.representation(using: .jpeg, properties: [.compressionFactor: 0.85]), out.count <= limit {
                return out
            }
            let size = NSSize(width: rep.pixelsWide / 2, height: rep.pixelsHigh / 2)
            guard size.width > 64, let smaller = resized(rep, to: size) else { return nil }
            rep = smaller
        }
        return nil
        #else
        guard var image = UIImage(data: data) else { return nil }
        for _ in 0..<4 {
            if let out = image.jpegData(compressionQuality: 0.85), out.count <= limit { return out }
            let size = CGSize(width: image.size.width / 2, height: image.size.height / 2)
            guard size.width > 64 else { return nil }
            image = UIGraphicsImageRenderer(size: size).image { _ in image.draw(in: CGRect(origin: .zero, size: size)) }
        }
        return nil
        #endif
    }

    #if os(macOS)
    private static func resized(_ rep: NSBitmapImageRep, to size: NSSize) -> NSBitmapImageRep? {
        guard let smaller = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: Int(size.width), pixelsHigh: Int(size.height), bitsPerSample: 8,
            samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0,
            bitsPerPixel: 0)
        else { return nil }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: smaller)
        rep.draw(in: NSRect(origin: .zero, size: size))
        NSGraphicsContext.restoreGraphicsState()
        return smaller
    }

    /// The clipboard's images, when it holds any.
    static func fromPasteboard() -> [Herder.Image] {
        let board = NSPasteboard.general
        if let urls = board.readObjects(forClasses: [NSURL.self], options: [.urlReadingContentsConformToTypes: [UTType.image.identifier]]) as? [URL],
           !urls.isEmpty {
            return urls.compactMap { url in
                (try? Data(contentsOf: url)).flatMap { try? make($0, type: UTType(filenameExtension: url.pathExtension)) }
            }
        }
        for type in [UTType.png, .jpeg, .tiff, .gif, .webP] {
            if let data = board.data(forType: NSPasteboard.PasteboardType(type.identifier)) {
                return (try? make(data, type: type)).map { [$0] } ?? []
            }
        }
        return []
    }
    #endif

    /// Loads dropped items' images.
    @MainActor static func load(_ providers: [NSItemProvider]) async -> [Herder.Image] {
        var images: [Herder.Image] = []
        for provider in providers where provider.hasItemConformingToTypeIdentifier(UTType.image.identifier) {
            let type = provider.registeredTypeIdentifiers.compactMap(UTType.init).first { $0.conforms(to: .image) }
            if let data = try? await provider.loadData(type: type ?? .image), let image = try? make(data, type: type) {
                images.append(image)
            }
        }
        return images
    }
}

extension NSItemProvider {
    @MainActor func loadData(type: UTType) async throws -> Data {
        try await withCheckedThrowingContinuation { continuation in
            // The provider answers on a queue of its own, not the main actor's.
            _ = loadDataRepresentation(for: type) { @Sendable data, error in
                if let data { continuation.resume(returning: data) } else {
                    continuation.resume(throwing: error ?? CocoaError(.fileReadUnknown))
                }
            }
        }
    }
}

/// Image bytes shown as a picture, with a fixed height.
struct Picture: View {
    let data: Data
    var height: CGFloat = 120

    var body: some View {
        #if os(macOS)
        if let image = NSImage(data: data) {
            SwiftUI.Image(nsImage: image).resizable().scaledToFit().frame(height: height)
                .clipShape(.rect(cornerRadius: 10))
        }
        #else
        if let image = UIImage(data: data) {
            SwiftUI.Image(uiImage: image).resizable().scaledToFit().frame(height: height)
                .clipShape(.rect(cornerRadius: 10))
        }
        #endif
    }
}

/// The images about to go with a prompt, each removable.
struct AttachmentStrip: View {
    let images: [Herder.Image]
    let remove: (Int) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(Array(images.enumerated()), id: \.offset) { index, image in
                    Picture(data: image.data, height: 64)
                        .overlay(alignment: .topTrailing) {
                            Button { remove(index) } label: {
                                SwiftUI.Image(systemName: "xmark.circle.fill")
                                    .foregroundStyle(Theme.text, Theme.background)
                            }
                            .buttonStyle(.plain)
                            .padding(3)
                        }
                }
            }
            .padding(.horizontal, 16)
            .padding(.top, 12)
        }
    }
}

/// A user message's images, fetched from the machine once.
struct MessageImages: View {
    let fleet: Fleet
    let key: SessionKey
    let attachments: [Attachment]

    var body: some View {
        HStack(spacing: 8) {
            ForEach(attachments, id: \.attachmentId) { attachment in
                Group {
                    if let data = fleet.attachments[attachment.attachmentId] {
                        Picture(data: data, height: 140)
                    } else {
                        RoundedRectangle(cornerRadius: 10).fill(Theme.raised)
                            .frame(width: 140, height: 100)
                            .overlay { ProgressView().controlSize(.small).tint(Theme.tertiary) }
                    }
                }
                .task { await fleet.fetchAttachment(attachment.attachmentId, of: key) }
            }
        }
    }
}
