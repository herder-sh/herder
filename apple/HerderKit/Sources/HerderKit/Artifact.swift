import AVKit
import Foundation
import Herder
import PDFKit
import SwiftUI

#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// What an agent published with herder's `publish` tool to show its work: a screenshot, a
/// recording, a page, a log. The thread renders the file, fetched from its machine, and offers
/// the public link, which opens on any device and can be sent to anyone.
struct Artifact: Hashable, Identifiable {
    enum Kind: Hashable { case image, video, page, pdf, text, file }

    /// The event's seq.
    let id: UInt64
    let title: String
    let attachment: Attachment
    /// `nil` when the project keeps its artifacts private.
    let url: URL?
    /// When `url` stops working; `nil` when it never does.
    let expiresAt: Date?

    init(id: UInt64, title: String, attachment: Attachment, url: String?, expiresAt: String?) {
        self.id = id
        self.title = title
        self.attachment = attachment
        self.url = url.flatMap(URL.init(string:))
        self.expiresAt = expiresAt.flatMap(Timestamp.date)
    }

    var name: String { attachment.name ?? "artifact" }

    private var pathExtension: String { (name as NSString).pathExtension.lowercased() }

    /// How it renders, by its name's extension, as the daemon picks its media type.
    var kind: Kind {
        switch pathExtension {
        case "png", "jpg", "jpeg", "gif", "webp", "heic": .image
        case "mp4", "m4v", "mov": .video
        case "html", "htm", "svg": .page
        case "pdf": .pdf
        case "txt", "log", "md", "markdown", "json", "csv", "diff", "patch", "xml", "yaml", "yml", "toml", "swift",
             "rs", "py", "js", "ts", "tsx", "jsx", "go", "java", "c", "cpp", "rb", "sh": .text
        default: .file
        }
    }

    /// The language `CodeHighlight` colours a text artifact as: its extension, which it names
    /// languages by; one it doesn't know stays plain.
    var language: String { pathExtension }

    /// What it is and how big, as its header says: "HTML · 24 KB".
    var summary: String {
        let type = pathExtension.isEmpty ? "File" : pathExtension.uppercased()
        return "\(type) · \(ByteCountFormatter.string(fromByteCount: Int64(attachment.size), countStyle: .file))"
    }

    /// The link to offer at `now`: `nil` once it has expired, or when there is none.
    func link(at now: Date) -> URL? {
        guard let url, expiresAt.map({ $0 > now }) ?? true else { return nil }
        return url
    }

    /// What its card offers.
    enum Action: Hashable { case open, download, copyLink, browser }

    /// The card's actions at `now`, in order: Open and Download always, from the file kept with
    /// the session; Copy link while the public link works; Open in browser with the link, or
    /// where the browser opens files (`opensFiles`), with the file.
    func actions(at now: Date, opensFiles: Bool) -> [Action] {
        let live = link(at: now) != nil
        return [.open, .download] + (live ? [.copyLink] : []) + (live || opensFiles ? [.browser] : [])
    }

    /// Where Download saves it in `folder`: its own name, numbered as the Finder numbers copies
    /// ("shot 2.png") when that is taken.
    func download(in folder: URL) -> URL {
        let base = (name as NSString).deletingPathExtension
        var file = folder.appendingPathComponent(name)
        var copy = 2
        while FileManager.default.fileExists(atPath: file.path) {
            let numbered = "\(base) \(copy)"
            file = folder.appendingPathComponent(pathExtension.isEmpty ? numbered
                                                     : "\(numbered).\((name as NSString).pathExtension)")
            copy += 1
        }
        return file
    }

    var symbol: String {
        switch kind {
        case .image: "photo"
        case .video: "play.rectangle"
        case .page: "macwindow"
        case .pdf: "doc.richtext"
        case .text: "doc.text"
        case .file: "doc"
        }
    }

    /// `data` as a file of its own name, for what opens files rather than bytes: the browser, a
    /// player, the share sheet. The same artifact always lands on the same file.
    func file(_ data: Data) throws -> URL {
        let file = FileManager.default.temporaryDirectory
            .appendingPathComponent("herder-artifacts", isDirectory: true)
            .appendingPathComponent(attachment.attachmentId, isDirectory: true)
            .appendingPathComponent(name)
        try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try data.write(to: file)
        return file
    }
}

extension Fleet {
    /// The artifacts `key`'s session published, oldest first.
    func artifacts(of key: SessionKey) -> [Artifact] {
        session(key)?.log.compactMap { if case .artifact(let artifact) = $0 { artifact } else { nil } } ?? []
    }
}

/// How far a viewer is zoomed: to fit the width, or by a scale of its own (1 is actual size).
enum Zoom: Hashable {
    case fit
    case scale(CGFloat)

    static let levels: [CGFloat] = [0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2, 2.5, 3, 4]

    /// The scale it shows at, where fitting takes `fit`: never below fit, never above 4×.
    func scale(fit: CGFloat) -> CGFloat {
        switch self {
        case .fit: fit
        case .scale(let scale): min(max(scale, fit), Self.levels[Self.levels.count - 1])
        }
    }

    /// The next level in from `scale`.
    static func larger(than scale: CGFloat) -> Zoom {
        .scale(levels.first { $0 > scale + 0.005 } ?? levels[levels.count - 1])
    }

    /// The next level out from `scale`.
    static func smaller(than scale: CGFloat) -> Zoom {
        .scale(levels.last { $0 < scale - 0.005 } ?? levels[0])
    }
}

/// A zoom the toolbar asks for; each press counts, even of the zoom already asked for, so Fit
/// width fits again after a pinch.
struct ZoomRequest: Equatable {
    var zoom: Zoom = .fit
    var id = 0
}

/// How wide a page lays out, in CSS pixels, and the scale that fits it.
enum PageLayout {
    /// What a page declares its layout width to be.
    enum Declared: Equatable {
        /// `width=device-width`, or a viewport without a width: the view's own width.
        case device
        case fixed(CGFloat)
    }

    /// A page without a viewport is a desktop page: it lays out this wide.
    static let defaultWidth: CGFloat = 1280

    /// `nil` when the page has no viewport meta tag.
    static func declared(_ html: String) -> Declared? {
        guard let meta = html.firstMatch(of: #/(?i)<meta\b[^>]*\bname\s*=\s*["']?viewport\b[^>]*>/#) else { return nil }
        guard let content = meta.output.firstMatch(of: #/(?i)\bcontent\s*=\s*["']([^"']*)["']/#),
              let width = content.output.1.firstMatch(of: #/(?i)\bwidth\s*=\s*([^,;\s]+)/#) else { return .device }
        if let pixels = Double(width.output.1), pixels > 0 { return .fixed(pixels) }
        return .device
    }

    /// The width it lays out at in a view `view` wide: what it declares, else the default, and
    /// wider when its content overflows that (`content`, its measured scroll width).
    static func width(_ declared: Declared?, view: CGFloat, content: CGFloat) -> CGFloat {
        let natural = switch declared ?? .fixed(defaultWidth) {
        case .device: view
        case .fixed(let width): width
        }
        return max(natural, content)
    }

    /// The scale that shows `width` whole across `view`: never above actual size.
    static func fit(view: CGFloat, width: CGFloat) -> CGFloat {
        guard view > 0, width > 0 else { return 1 }
        return min(1, view / width)
    }
}

/// An artifact as a card in the thread: a sharp thumbnail of it at fit width, then its title,
/// type and size with Open, Download, Copy link and Open in browser. The thumbnail opens the
/// viewer too.
struct ArtifactBlock: View {
    let artifact: Artifact
    let fleet: Fleet
    let key: SessionKey
    @State private var height: CGFloat = 0
    #if os(macOS)
    @Environment(\.openWindow) private var openWindow
    #else
    @State private var viewing = false
    #endif
    @Environment(\.openURL) private var openURL
    @State private var copied = false
    #if os(iOS)
    /// The file Download shares.
    @State private var file: URL?
    #endif

    private static let cap: CGFloat = 360

    private var data: Data? { fleet.attachments[artifact.attachment.attachmentId] }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if artifact.kind != .file {
                thumbnail
                Rectangle().fill(Theme.stroke).frame(height: 1)
            }
            footer
        }
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .clipShape(.rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
        .task { await fleet.fetchAttachment(artifact.attachment.attachmentId, of: key) }
        #if os(iOS)
        .fullScreenCover(isPresented: $viewing) { ArtifactViewer(fleet: fleet, key: key, start: artifact.id) }
        #endif
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("artifact")
    }

    private func open() {
        #if os(macOS)
        openWindow(value: ArtifactViewer.Ref(key: key, id: artifact.id))
        #else
        viewing = true
        #endif
    }

    @ViewBuilder private var thumbnail: some View {
        if let data {
            switch artifact.kind {
            case .image:
                Picture(data: data, height: 280)
                    .frame(maxWidth: .infinity)
                    .padding(8)
                    .opening(open)
            case .video:
                ArtifactVideo(artifact: artifact, data: data)
                    .frame(height: 280)
                    .clipShape(.rect(cornerRadius: Theme.corner))
                    .padding(8)
            case .page:
                // A picture of the page, not a page to use: swipes scroll the thread, a click opens it.
                SandboxedWebView(html: String(decoding: data, as: UTF8.self), thumbnail: true, measured: { height = $0 })
                    .frame(height: height > 0 ? min(height, Self.cap) : 160)
                    .allowsHitTesting(false)
                    .opening(open)
            case .pdf:
                PDFThumbnail(data: data)
                    .frame(maxWidth: .infinity)
                    .padding(8)
                    .opening(open)
            case .text:
                Text(CodeHighlight.attributed(String(decoding: data.prefix(4 * 1024), as: UTF8.self),
                                              language: artifact.language))
                    .font(.caption.monospaced())
                    .foregroundStyle(Theme.text)
                    .lineLimit(14)
                    .frame(maxWidth: .infinity, alignment: .topLeading)
                    .padding(12)
                    .opening(open)
            case .file:
                EmptyView()
            }
        } else {
            ProgressView().controlSize(.small).tint(Theme.tertiary)
                .frame(maxWidth: .infinity, minHeight: 72)
        }
    }

    /// The title gets the card's width: beside the buttons where it fits whole, else above them;
    /// where even the buttons don't fit, all but Open fold into a ⋯ menu.
    private var footer: some View {
        TimelineView(.periodic(from: .now, by: 60)) { context in
            let link = artifact.link(at: context.date)
            let actions = artifact.actions(at: context.date, opensFiles: ArtifactBrowser.opensFiles)
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 10) {
                    heading(link: link).fixedSize(horizontal: true, vertical: false)
                    Spacer(minLength: 0)
                    buttons(actions, link: link)
                }
                VStack(alignment: .leading, spacing: 8) {
                    heading(link: link)
                    buttons(actions, link: link).padding(.leading, 16)
                }
                VStack(alignment: .leading, spacing: 8) {
                    heading(link: link)
                    HStack(spacing: HeaderControl.spacing) {
                        button(.open, link: link)
                        Menu {
                            ForEach(actions.dropFirst(), id: \.self) { item($0, link: link) }
                        } label: {
                            HeaderLabel(symbol: "ellipsis")
                        }
                        .modifier(HeaderMenuStyle())
                        .accessibilityLabel("More")
                    }
                    .padding(.leading, 16)
                }
            }
            .padding(.leading, 12).padding(.trailing, 8).padding(.vertical, 8)
            .contextMenu {
                if let link {
                    Button("Copy Link", systemImage: "link") { Clipboard.string = link.absoluteString }
                    ShareLink(item: link, subject: Text(artifact.title))
                }
            }
        }
        #if os(iOS)
        .task(id: data != nil) { file = data.flatMap { try? artifact.file($0) } }
        #endif
    }

    private func heading(link: URL?) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: artifact.symbol).font(.callout).foregroundStyle(Theme.tertiary).frame(width: 16)
            VStack(alignment: .leading, spacing: 1) {
                Text(artifact.title).font(.callout.weight(.medium)).foregroundStyle(Theme.text)
                Text(meta(link: link)).font(.caption).foregroundStyle(Theme.tertiary).lineLimit(1)
            }
        }
    }

    private func buttons(_ actions: [Artifact.Action], link: URL?) -> some View {
        HStack(spacing: HeaderControl.spacing) {
            ForEach(actions, id: \.self) { button($0, link: link) }
        }
    }

    @ViewBuilder private func button(_ action: Artifact.Action, link: URL?) -> some View {
        switch action {
        case .download:
            #if os(iOS)
            // iOS saves through the share sheet: Save to Files, Save Image.
            if let file {
                ShareLink(item: file) { HeaderLabel(symbol: "square.and.arrow.down", title: "Download") }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("artifact-download")
            } else {
                HeaderButton(symbol: "square.and.arrow.down", title: "Download") {}.disabled(true)
            }
            #else
            HeaderButton(symbol: "square.and.arrow.down", title: "Download", action: download)
                .disabled(data == nil)
                .help(help(.download, link: nil))
                .accessibilityIdentifier("artifact-download")
            #endif
        default:
            HeaderButton(symbol: symbol(action), title: title(action)) { perform(action, link: link) }
                .disabled(action == .open && data == nil)
                .help(help(action, link: link))
                .accessibilityIdentifier("artifact-\(action)")
        }
    }

    /// `action` as an item of the ⋯ menu.
    @ViewBuilder private func item(_ action: Artifact.Action, link: URL?) -> some View {
        #if os(iOS)
        if action == .download, let file {
            ShareLink(item: file) { Label("Download", systemImage: "square.and.arrow.down") }
        } else if action != .download {
            Button(title(action), systemImage: symbol(action)) { perform(action, link: link) }
        }
        #else
        Button(title(action), systemImage: symbol(action)) { perform(action, link: link) }
            .disabled(action == .download && data == nil)
        #endif
    }

    private func title(_ action: Artifact.Action) -> String {
        switch action {
        case .open: "Open"
        case .download: "Download"
        case .copyLink: copied ? "Copied" : "Copy link"
        case .browser: "Open in browser"
        }
    }

    private func symbol(_ action: Artifact.Action) -> String {
        switch action {
        case .open: "arrow.up.left.and.arrow.down.right"
        case .download: "square.and.arrow.down"
        case .copyLink: copied ? "checkmark" : "link"
        case .browser: "arrow.up.right.square"
        }
    }

    private func help(_ action: Artifact.Action, link: URL?) -> String {
        switch action {
        case .open: "Open in the viewer"
        case .download: "Save to Downloads"
        case .copyLink: "Copy the public link"
        case .browser: link?.absoluteString ?? "Open a copy of the file"
        }
    }

    private func perform(_ action: Artifact.Action, link: URL?) {
        switch action {
        case .open: open()
        case .download: download()
        case .copyLink:
            Clipboard.string = link?.absoluteString
            copied = true
        case .browser:
            if let link { openURL(link) } else if let data { ArtifactBrowser.open(artifact, data) }
        }
    }

    /// Saves the file to the Mac's Downloads, saying so with Show in Finder.
    private func download() {
        #if os(macOS)
        guard let data else { return }
        do {
            let folder = try FileManager.default.url(for: .downloadsDirectory, in: .userDomainMask,
                                                     appropriateFor: nil, create: false)
            let file = artifact.download(in: folder)
            try data.write(to: file, options: .withoutOverwriting)
            fleet.toast = Toast(text: "Saved to Downloads", reveal: file)
        } catch {
            fleet.toast = .failure("Could not save \(artifact.name)", error)
        }
        #endif
    }

    private func meta(link: URL?) -> String {
        let reach = link != nil ? "Public link" : artifact.url == nil ? "Private" : "Link expired"
        return "\(artifact.summary) · \(reach)"
    }
}

private extension View {
    /// Opens the viewer on a click anywhere on it.
    func opening(_ open: @escaping () -> Void) -> some View {
        contentShape(.rect).onTapGesture(perform: open).accessibilityAddTraits(.isButton)
    }
}

/// Opens a private artifact, which has no link, in the browser: the Mac opens its file with the
/// default app; iOS opens no local files, so it offers the share sheet instead.
enum ArtifactBrowser {
    #if os(macOS)
    static let opensFiles = true
    #else
    static let opensFiles = false
    #endif

    @MainActor static func open(_ artifact: Artifact, _ data: Data) {
        #if os(macOS)
        guard let file = try? artifact.file(data) else { return }
        NSWorkspace.shared.open(file)
        #endif
    }
}

/// A PDF's first page, as the card's thumbnail.
private struct PDFThumbnail: View {
    let data: Data

    var body: some View {
        if let page = PDFDocument(data: data)?.page(at: 0) {
            #if os(macOS)
            SwiftUI.Image(nsImage: page.thumbnail(of: CGSize(width: 900, height: 900), for: .mediaBox))
                .resizable().scaledToFit().frame(maxHeight: 280)
            #else
            SwiftUI.Image(uiImage: page.thumbnail(of: CGSize(width: 900, height: 900), for: .mediaBox))
                .resizable().scaledToFit().frame(maxHeight: 280)
            #endif
        }
    }
}

/// A recording, played from a file of its own: AVPlayer plays files, not bytes.
struct ArtifactVideo: View {
    let artifact: Artifact
    let data: Data
    @State private var player: AVPlayer?

    var body: some View {
        Group {
            if let player { VideoPlayer(player: player) } else { Theme.raised }
        }
        .task(id: data) { player = (try? artifact.file(data)).map(AVPlayer.init(url:)) }
    }
}
