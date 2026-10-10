import AVKit
import Foundation
import Herder
import SwiftUI
import WebKit

#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// What an agent published with herder's `publish` tool to show its work: a screenshot, a
/// recording, a page, a log. The thread renders the file, fetched from its machine, and offers
/// the public link, which opens on any device and can be sent to anyone.
struct Artifact: Hashable, Identifiable {
    enum Kind: Hashable { case image, video, page, text, file }

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

    /// How it renders, by its name's extension, as the daemon picks its media type.
    var kind: Kind {
        switch (name as NSString).pathExtension.lowercased() {
        case "png", "jpg", "jpeg", "gif", "webp", "heic": .image
        case "mp4", "m4v", "mov": .video
        case "html", "htm", "svg": .page
        case "txt", "log", "md", "markdown", "json", "csv", "diff", "patch", "xml", "yaml", "yml": .text
        default: .file
        }
    }

    /// The link to offer at `now`: `nil` once it has expired, or when there is none.
    func link(at now: Date) -> URL? {
        guard let url, expiresAt.map({ $0 > now }) ?? true else { return nil }
        return url
    }

    var symbol: String {
        switch kind {
        case .image: "photo"
        case .video: "play.rectangle"
        case .page: "macwindow"
        case .text: "doc.text"
        case .file: "doc"
        }
    }
}

/// No network at all: scripts and styles inline, images, fonts and media from data only.
enum SandboxedPage {
    static let policy =
        "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:"

    /// The page with the policy and the dark colour scheme ahead of anything it holds, after its
    /// doctype so it keeps standards mode.
    static func wrapped(_ html: String) -> String {
        let head = """
            <meta http-equiv="Content-Security-Policy" content="\(policy)">
            <meta name="color-scheme" content="dark">
            <meta name="viewport" content="width=device-width, initial-scale=1">
            <style>:root { color-scheme: dark; }</style>

            """
        let page = html.drop(while: \.isWhitespace)
        if page.prefix(9).lowercased() == "<!doctype", let end = page.firstIndex(of: ">") {
            return String(page[...end]) + "\n" + head + String(page[page.index(after: end)...])
        }
        return head + String(page)
    }
}

/// An artifact as a card: its title, the file rendered as what it is, and its link to copy,
/// share or open. A page or picture sizes the card up to `cap`; on iOS a taller one is cut there
/// so a swipe on it always scrolls the thread, and the larger view shows it all.
struct ArtifactBlock: View {
    let artifact: Artifact
    let fleet: Fleet
    let key: SessionKey
    @State private var height: CGFloat = 0
    @State private var expanded = false
    @State private var copied = false

    private static let cap: CGFloat = 600

    private var data: Data? { fleet.attachments[artifact.attachment.attachmentId] }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 2) {
                Image(systemName: artifact.symbol).font(.caption).foregroundStyle(Theme.tertiary)
                Text(artifact.title).font(.caption.weight(.medium)).foregroundStyle(Theme.secondary).lineLimit(1)
                    .padding(.leading, 4)
                Spacer()
                if data != nil, [.image, .page, .text].contains(artifact.kind) {
                    action("arrow.up.left.and.arrow.down.right", "Expand") { expanded = true }
                }
            }
            .padding(.leading, 12).padding(.trailing, 6).padding(.vertical, 6)
            content
            Rectangle().fill(Theme.stroke).frame(height: 1)
            linkLine
        }
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
        .task { await fleet.fetchAttachment(artifact.attachment.attachmentId, of: key) }
        .task(id: copied) {
            guard copied else { return }
            try? await Task.sleep(for: .seconds(1.5))
            copied = false
        }
        .sheet(isPresented: $expanded) {
            if let data { ArtifactExpanded(artifact: artifact, data: data) }
        }
        .accessibilityIdentifier("artifact")
    }

    @ViewBuilder private var content: some View {
        if let data {
            switch artifact.kind {
            case .image:
                Picture(data: data, height: 280)
                    .frame(maxWidth: .infinity)
                    .padding(8)
                    .onTapGesture { expanded = true }
            case .video:
                ArtifactVideo(name: artifact.name, data: data)
                    .frame(height: 280)
                    .clipShape(.rect(cornerRadius: Theme.corner))
                    .padding(8)
            case .page:
                SandboxedWebView(html: String(decoding: data, as: UTF8.self), inline: true) { height = $0 }
                    .frame(height: height > 0 ? min(height, Self.cap) : 160)
                    .clipShape(.rect(cornerRadius: Theme.corner))
                    #if os(iOS)
                    .overlay(alignment: .bottom) {
                        if height > Self.cap {
                            Button("Show All") { expanded = true }
                                .font(.caption.weight(.medium))
                                .buttonStyle(.bordered)
                                .padding(8)
                        }
                    }
                    #endif
            case .text:
                ScrollView {
                    Text(String(decoding: data.prefix(64 * 1024), as: UTF8.self))
                        .font(.caption.monospaced())
                        .foregroundStyle(Theme.text)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(12)
                }
                .frame(maxHeight: 280)
                .fixedSize(horizontal: false, vertical: true)
            case .file:
                Label("\(artifact.name) · \(ByteCountFormatter.string(fromByteCount: Int64(data.count), countStyle: .file))",
                      systemImage: "doc")
                    .font(.footnote).foregroundStyle(Theme.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(12)
            }
        } else {
            ProgressView().controlSize(.small).tint(Theme.tertiary)
                .frame(maxWidth: .infinity, minHeight: 72)
        }
    }

    /// The public link with copy, share and open; or why there is none.
    @ViewBuilder private var linkLine: some View {
        TimelineView(.periodic(from: .now, by: 60)) { context in
            HStack(spacing: 2) {
                if let url = artifact.link(at: context.date) {
                    Link(destination: url) {
                        Text(url.absoluteString).font(.caption).foregroundStyle(Theme.link).lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Spacer(minLength: 8)
                    action(copied ? "checkmark" : "link", copied ? "Copied" : "Copy link") {
                        Clipboard.string = url.absoluteString
                        copied = true
                    }
                    ShareLink(item: url, subject: Text(artifact.title)) {
                        Image(systemName: "square.and.arrow.up")
                            .font(.caption.weight(.medium))
                            .foregroundStyle(Theme.tertiary)
                            .frame(width: 24, height: 22)
                            .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .help("Share link")
                    .accessibilityLabel("Share link")
                } else {
                    Text(artifact.url == nil ? "Private: this project's artifacts get no public link" : "Link expired")
                        .font(.caption).foregroundStyle(Theme.tertiary)
                    Spacer()
                }
            }
            .padding(.leading, 12).padding(.trailing, 6).padding(.vertical, 6)
        }
    }

    private func action(_ symbol: String, _ help: String, _ perform: @escaping () -> Void) -> some View {
        Button(action: perform) {
            Image(systemName: symbol)
                .font(.caption.weight(.medium))
                .foregroundStyle(Theme.tertiary)
                .frame(width: 24, height: 22)
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .help(help)
        .accessibilityLabel(help)
    }
}

/// A recording, played from a file of its own: AVPlayer plays files, not bytes.
private struct ArtifactVideo: View {
    let name: String
    let data: Data
    @State private var player: AVPlayer?

    var body: some View {
        Group {
            if let player { VideoPlayer(player: player) } else { Theme.raised }
        }
        .task(id: data) {
            let file = FileManager.default.temporaryDirectory
                .appendingPathComponent("herder-artifacts", isDirectory: true)
                .appendingPathComponent(UUID().uuidString, isDirectory: true)
                .appendingPathComponent(name)
            do {
                try FileManager.default.createDirectory(at: file.deletingLastPathComponent(),
                                                        withIntermediateDirectories: true)
                try data.write(to: file)
                player = AVPlayer(url: file)
            } catch {
                player = nil
            }
        }
    }
}

/// The artifact filling a sheet, scrolling as a whole.
private struct ArtifactExpanded: View {
    let artifact: Artifact
    let data: Data
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Text(artifact.title).font(.headline).foregroundStyle(Theme.text).lineLimit(1)
                Spacer()
                IconButton(symbol: "xmark", help: "Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding(16)
            Rectangle().fill(Theme.stroke).frame(height: 1)
            switch artifact.kind {
            case .page:
                SandboxedWebView(html: String(decoding: data, as: UTF8.self), inline: false) { _ in }
            case .image:
                ScrollView([.horizontal, .vertical]) { Picture(data: data, height: 900) }
            default:
                ScrollView {
                    Text(String(decoding: data, as: UTF8.self))
                        .font(.caption.monospaced()).foregroundStyle(Theme.text).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(16)
                }
            }
        }
        .background(Theme.surface)
        #if os(macOS)
        .frame(minWidth: 640, idealWidth: 960, minHeight: 440, idealHeight: 680)
        #endif
        .preferredColorScheme(.dark)
    }
}

/// Runs an agent's page sandboxed: no storage that outlives it, a policy that blocks every
/// network load, and only the page itself may load in it. A link the user follows opens in the
/// browser; popups, dialogs and any other navigation do nothing.
@MainActor
struct SandboxedWebView {
    let html: String
    let inline: Bool
    /// The page's height, as it lays out and on every resize.
    let measured: (CGFloat) -> Void

    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate, WKUIDelegate, WKScriptMessageHandler {
        var html = ""
        var measured: (CGFloat) -> Void = { _ in }
        var open: (URL) -> Void = { _ in }
        /// The load of `html` has been let through; nothing else will be.
        private var started = false

        func load(_ html: String, in web: WKWebView) {
            self.html = html
            started = false
            web.loadHTMLString(SandboxedPage.wrapped(html), baseURL: nil)
        }

        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
            if !started, action.navigationType == .other, action.targetFrame?.isMainFrame == true {
                started = true
                return .allow
            }
            if action.navigationType == .linkActivated, let url = action.request.url,
               ["http", "https"].contains(url.scheme?.lowercased()) {
                open(url)
            }
            return .cancel
        }

        func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                     for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? { nil }

        func webView(_ webView: WKWebView, runJavaScriptAlertPanelWithMessage message: String,
                     initiatedByFrame frame: WKFrameInfo) async {}

        func webView(_ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String,
                     initiatedByFrame frame: WKFrameInfo) async -> Bool { false }

        func webView(_ webView: WKWebView, runJavaScriptTextInputPanelWithPrompt prompt: String, defaultText: String?,
                     initiatedByFrame frame: WKFrameInfo) async -> String? { nil }

        func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
            if let height = message.body as? Double, height.isFinite, height > 0 { measured(CGFloat(height)) }
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    /// Posts the page's height from the client world, where the page can't reach the handler.
    private static let measure = """
        (() => {
          const post = () => {
            const height = Math.ceil(document.documentElement.getBoundingClientRect().height);
            window.webkit.messageHandlers.height.postMessage(height);
          };
          const observer = new ResizeObserver(post);
          observer.observe(document.documentElement);
          if (document.body) observer.observe(document.body);
          addEventListener("load", post);
          post();
        })();
        """

    private func makeWebView(_ coordinator: Coordinator) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        config.preferences.javaScriptCanOpenWindowsAutomatically = false
        let scripts = config.userContentController
        scripts.add(coordinator, contentWorld: .defaultClient, name: "height")
        scripts.addUserScript(WKUserScript(source: Self.measure, injectionTime: .atDocumentEnd, forMainFrameOnly: true,
                                           in: .defaultClient))
        let web = WKWebView(frame: .zero, configuration: config)
        web.navigationDelegate = coordinator
        web.uiDelegate = coordinator
        #if os(iOS)
        web.overrideUserInterfaceStyle = .dark
        web.isOpaque = false
        web.backgroundColor = .clear
        web.scrollView.backgroundColor = .clear
        // Inline, the thread scrolls under a swipe; the page is cut at the card's height.
        web.scrollView.isScrollEnabled = !inline
        #else
        web.appearance = NSAppearance(named: .darkAqua)
        // No public API for a transparent WKWebView on the Mac; this key is the standard one.
        web.setValue(false, forKey: "drawsBackground")
        #endif
        return web
    }

    private func update(_ web: WKWebView, _ coordinator: Coordinator, _ openURL: OpenURLAction) {
        coordinator.measured = measured
        coordinator.open = { openURL($0) }
        guard coordinator.html != html else { return }
        coordinator.load(html, in: web)
    }
}

#if os(iOS)
extension SandboxedWebView: UIViewRepresentable {
    func makeUIView(context: Context) -> WKWebView { makeWebView(context.coordinator) }
    func updateUIView(_ web: WKWebView, context: Context) { update(web, context.coordinator, context.environment.openURL) }
}
#else
extension SandboxedWebView: NSViewRepresentable {
    func makeNSView(context: Context) -> WKWebView { makeWebView(context.coordinator) }
    func updateNSView(_ web: WKWebView, context: Context) { update(web, context.coordinator, context.environment.openURL) }
}
#endif
