import Foundation
import Herder
import SwiftUI
import WebKit

#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// A self-contained page an agent showed with herder's `show_html` tool: a chart, a code map, a
/// UI mock. It renders live in the thread, where the call happened, as in T3 Code's visual
/// replies.
struct HtmlVisual: Hashable, Identifiable {
    let id: String
    let title: String
    /// The page; `nil` while the call streams in, or when its input has none.
    let html: String?
    /// The call's input is still streaming in.
    let building: Bool

    init(id: String, input: Json, streaming: Bool) {
        let object = streaming ? nil : (try? JSONSerialization.jsonObject(with: Data(input.utf8))) as? [String: Any]
        let title = (object?["title"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let html = object?["html"] as? String
        self.id = id
        self.title = title.isEmpty ? "Visual" : title
        self.html = html?.isEmpty == false ? html : nil
        building = streaming
    }

    /// The tool's name as each provider calls it: `mcp__herder__show_html` for Claude,
    /// `herder.show_html` for Codex, and so on.
    static func isShowHtml(_ name: String) -> Bool {
        name == "show_html" || ["__", ".", "-"].contains { name.hasSuffix("\($0)show_html") }
    }

    /// No network at all: scripts and styles inline, images, fonts and media from data only.
    static let policy =
        "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:"

    /// The page with the policy and the dark colour scheme ahead of anything it holds, after its
    /// doctype so it keeps standards mode.
    static func sandboxed(_ html: String) -> String {
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

/// A `show_html` call as a card, like `MermaidBlock`: the title, a toggle to the source, copy,
/// and a larger view. The page sizes the card up to `cap`; on the Mac a taller one scrolls inside
/// it, on iOS it is cut there so a swipe on it always scrolls the thread, and the larger view
/// shows it all.
struct HtmlVisualBlock: View {
    let visual: HtmlVisual
    @State private var height: CGFloat = 0
    @State private var showCode = false
    @State private var expanded = false
    @State private var copied = false

    private static let cap: CGFloat = 600

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 2) {
                Image(systemName: "macwindow").font(.caption).foregroundStyle(Theme.tertiary)
                Text(visual.title).font(.caption.weight(.medium)).foregroundStyle(Theme.secondary).lineLimit(1)
                    .padding(.leading, 4)
                Spacer()
                if visual.html != nil {
                    action(showCode ? "macwindow" : "chevron.left.forwardslash.chevron.right",
                           showCode ? "Show visual" : "Show code") { showCode.toggle() }
                    if !showCode {
                        action("arrow.up.left.and.arrow.down.right", "Expand visual") { expanded = true }
                    }
                    action(copied ? "checkmark" : "doc.on.doc", copied ? "Copied" : "Copy source") {
                        Clipboard.string = visual.html
                        copied = true
                    }
                }
            }
            .padding(.leading, 12).padding(.trailing, 6).padding(.vertical, 6)
            content
        }
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
        .task(id: copied) {
            guard copied else { return }
            try? await Task.sleep(for: .seconds(1.5))
            copied = false
        }
        .sheet(isPresented: $expanded) {
            if let html = visual.html { HtmlVisualExpanded(title: visual.title, html: html) }
        }
    }

    @ViewBuilder private var content: some View {
        if let html = visual.html {
            if showCode {
                MarkdownText.codeText(html, language: "html")
            } else {
                HtmlVisualWebView(html: html, inline: true) { height = $0 }
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
            }
        } else {
            Text(visual.building ? "Building visual…" : "The visual has no page to show.")
                .font(.caption).foregroundStyle(Theme.tertiary)
                .frame(maxWidth: .infinity, minHeight: 72)
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

/// The page filling a sheet, scrolling as a whole.
private struct HtmlVisualExpanded: View {
    let title: String
    let html: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Text(title).font(.headline).foregroundStyle(Theme.text).lineLimit(1)
                Spacer()
                IconButton(symbol: "doc.on.doc", help: "Copy source") { Clipboard.string = html }
                IconButton(symbol: "xmark", help: "Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding(16)
            Rectangle().fill(Theme.stroke).frame(height: 1)
            HtmlVisualWebView(html: html, inline: false) { _ in }
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
struct HtmlVisualWebView {
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
            web.loadHTMLString(HtmlVisual.sandboxed(html), baseURL: nil)
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
extension HtmlVisualWebView: UIViewRepresentable {
    func makeUIView(context: Context) -> WKWebView { makeWebView(context.coordinator) }
    func updateUIView(_ web: WKWebView, context: Context) { update(web, context.coordinator, context.environment.openURL) }
}
#else
extension HtmlVisualWebView: NSViewRepresentable {
    func makeNSView(context: Context) -> WKWebView { makeWebView(context.coordinator) }
    func updateNSView(_ web: WKWebView, context: Context) { update(web, context.coordinator, context.environment.openURL) }
}
#endif
