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

    /// Whether a pasteboard holds an image, by its types alone, without reading it.
    static func available(_ board: NSPasteboard = .general) -> Bool {
        board.availableType(from: [.png, .tiff, NSPasteboard.PasteboardType(UTType.jpeg.identifier),
                                   NSPasteboard.PasteboardType(UTType.gif.identifier),
                                   NSPasteboard.PasteboardType(UTType.webP.identifier)]) != nil
            || board.canReadObject(forClasses: [NSURL.self], options: [.urlReadingContentsConformToTypes: [UTType.image.identifier]])
    }

    /// A pasteboard's images, when it holds any: the clipboard's, or a drag's.
    static func from(_ board: NSPasteboard = .general) -> [Herder.Image] {
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
    #else
    /// The clipboard's images, when it holds any. Items copied as bytes come as they are;
    /// those copied as pictures, as PNG.
    static func from(_ board: UIPasteboard) -> [Herder.Image] {
        let images = images(in: board.items)
        if !images.isEmpty { return images }
        return (board.images ?? []).compactMap { picture in
            picture.pngData().flatMap { try? make($0, type: .png) }
        }
    }
    #endif

    /// The images among pasteboard items, one per item that holds an image's bytes: in a type
    /// the daemon takes if the item has one, else in another to be converted.
    static func images(in items: [[String: Any]]) -> [Herder.Image] {
        let accepted = Set(imageMediaTypes())
        func taken(_ type: UTType) -> Bool { type.preferredMIMEType.map { accepted.contains($0) } ?? false }
        return items.compactMap { item in
            let candidates = item.compactMap { entry -> (type: UTType, data: Data)? in
                guard let type = UTType(entry.key), type.conforms(to: .image), let data = entry.value as? Data
                else { return nil }
                return (type, data)
            }
            .sorted { a, b in
                taken(a.type) != taken(b.type) ? taken(a.type) : a.type.identifier < b.type.identifier
            }
            for candidate in candidates {
                if let image = try? make(candidate.data, type: candidate.type) { return image }
            }
            return nil
        }
    }

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

/// Files of any type for a prompt, which the agent reads on the session's machine under their
/// own names, within the daemon's limits (`maxFileBytes()`, `maxFileNameBytes()`,
/// `maxPromptAttachmentBytes()`).
enum FileAttachment {
    /// A file from where it is on this device: picked, imported or dropped.
    static func make(_ url: URL) throws -> PromptFile {
        let name = name(url.lastPathComponent)
        guard let data = try? Data(contentsOf: url) else {
            throw ImageAttachment.Refused(errorDescription: "\(name) cannot be read.")
        }
        return try make(name: name, data: data)
    }

    /// A file from its name and bytes.
    static func make(name: String, data: Data) throws -> PromptFile {
        let limit = Int64(maxFileBytes())
        guard data.count <= limit else {
            throw ImageAttachment.Refused(errorDescription: "\(name) is over \(size(limit)), too large to send.")
        }
        return PromptFile(name: self.name(name), data: data)
    }

    /// Files and images picked or dropped together: a picture as an image the agent sees,
    /// anything else, or a picture that cannot go as an image, as a file. Says why one could not
    /// be taken.
    static func sort(_ urls: [URL]) -> (images: [Herder.Image], files: [PromptFile], refused: String?) {
        var images: [Herder.Image] = []
        var files: [PromptFile] = []
        var refused: String?
        for url in urls {
            let type = UTType(filenameExtension: url.pathExtension)
            if type?.conforms(to: .image) == true, let data = try? Data(contentsOf: url),
               let image = try? ImageAttachment.make(data, type: type) {
                images.append(image)
                continue
            }
            do { files.append(try make(url)) } catch { refused = error.localizedDescription }
        }
        return (images, files, refused)
    }

    /// Of `added`, those that fit in a prompt already carrying `carried` bytes of images and
    /// files, in order, and why the rest do not.
    static func fitting(_ added: [PromptFile], carried: Int) -> (files: [PromptFile], refused: String?) {
        let limit = Int(maxPromptAttachmentBytes())
        var total = carried
        var kept: [PromptFile] = []
        var refused: String?
        for file in added {
            guard total + file.data.count <= limit else {
                refused = "\(file.name) would take the prompt over \(size(Int64(limit))) of attachments."
                continue
            }
            total += file.data.count
            kept.append(file)
        }
        return (kept, refused)
    }

    /// `raw` as the daemon takes a file's name: no folders, no control characters, within its
    /// byte limit, keeping the extension when it is cut.
    static func name(_ raw: String) -> String {
        var name = String(raw.map { character -> Character in
            let control = character.unicodeScalars.contains { $0.properties.generalCategory == .control }
            return character == "/" || character == "\\" || control ? "_" : character
        })
        let limit = Int(maxFileNameBytes())
        if name.utf8.count > limit {
            let ext = (name as NSString).pathExtension
            var stem = (name as NSString).deletingPathExtension
            let suffix = ext.isEmpty || ext.utf8.count >= limit / 2 ? "" : "." + ext
            while !stem.isEmpty && stem.utf8.count + suffix.utf8.count > limit { stem.removeLast() }
            name = stem + suffix
        }
        return name.isEmpty || name == "." || name == ".." ? "file" : name
    }

    static func size(_ bytes: Int64) -> String { ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file) }

    /// The symbol that stands for a file of this name's type.
    static func symbol(_ name: String) -> String {
        guard let type = UTType(filenameExtension: (name as NSString).pathExtension) else { return "doc" }
        if type.conforms(to: .pdf) { return "doc.richtext" }
        if type.conforms(to: .spreadsheet) || type.conforms(to: .commaSeparatedText) { return "tablecells" }
        if type.conforms(to: .archive) { return "doc.zipper" }
        if type.conforms(to: .image) { return "photo" }
        if type.conforms(to: .sourceCode) { return "chevron.left.forwardslash.chevron.right" }
        if type.conforms(to: .text) { return "doc.text" }
        return "doc"
    }

    /// Loads dropped items' files: each one that is a file on this device, by its URL.
    @MainActor static func urls(_ providers: [NSItemProvider]) async -> [URL] {
        var urls: [URL] = []
        for provider in providers where provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) {
            if let url = try? await provider.loadFileURL() { urls.append(url) }
        }
        return urls
    }
}

/// A file going with a prompt, or sent with one: its type's symbol, name and size.
struct FileChip: View {
    let name: String
    let size: Int64
    /// Takes it off the prompt; absent once it is sent.
    var remove: (() -> Void)?

    var body: some View {
        HStack(spacing: 6) {
            SwiftUI.Image(systemName: FileAttachment.symbol(name)).foregroundStyle(Theme.secondary)
            Text(name).lineLimit(1).truncationMode(.middle).foregroundStyle(Theme.text)
            Text(FileAttachment.size(size)).foregroundStyle(Theme.tertiary).fixedSize()
            if let remove {
                Button(action: remove) {
                    SwiftUI.Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.tertiary)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Remove \(name)")
            }
        }
        .font(.footnote)
        .padding(.horizontal, 10)
        .frame(maxWidth: 260, minHeight: 30)
        .background(Theme.raised, in: .capsule)
        .accessibilityElement(children: .combine)
    }
}

/// The files about to go with a prompt, each removable.
struct FileStrip: View {
    let files: [PromptFile]
    let remove: (Int) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(Array(files.enumerated()), id: \.offset) { index, file in
                    FileChip(name: file.name, size: Int64(file.data.count)) { remove(index) }
                }
            }
            .padding(.horizontal, 16)
            .padding(.top, 12)
        }
        .accessibilityIdentifier("file-strip")
    }
}

/// The files a user message carried, as chips; the agent read them on the machine.
struct MessageFiles: View {
    /// Name and size of each.
    let files: [(name: String, size: Int64)]

    var body: some View {
        FlowLayout(spacing: 6, lineSpacing: 6) {
            ForEach(Array(files.enumerated()), id: \.offset) { _, file in
                FileChip(name: file.name, size: file.size)
            }
        }
    }
}

extension NSItemProvider {
    /// The URL of a dropped file.
    @MainActor func loadFileURL() async throws -> URL {
        try await withCheckedThrowingContinuation { continuation in
            _ = loadObject(ofClass: URL.self) { @Sendable url, error in
                if let url { continuation.resume(returning: url) } else {
                    continuation.resume(throwing: error ?? CocoaError(.fileReadUnknown))
                }
            }
        }
    }

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
    /// The message's images, not its files.
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

extension Attachment {
    /// Whether it is a file the agent read on the machine, rather than an image it saw.
    var isFile: Bool { name != nil }
}
