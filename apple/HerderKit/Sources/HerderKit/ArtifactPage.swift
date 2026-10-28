import Foundation
import SwiftUI
import WebKit

/// No network at all: scripts and styles inline, images, fonts and media from data only.
enum SandboxedPage {
    static let policy =
        "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:"

    /// The page with the policy and the dark colour scheme ahead of anything it holds, after its
    /// doctype so it keeps standards mode. A page that declares no viewport gets the desktop
    /// width it was made at, which iOS then fits to the screen; the Mac ignores viewports and
    /// fits it with its page zoom instead.
    /// The viewport herder gives a page without one, which iOS widens to content wider still.
    static let viewportId = "herder-viewport"

    static func wrapped(_ html: String) -> String {
        let viewport = PageLayout.declared(html) == nil
            ? "<meta name=\"viewport\" id=\"\(viewportId)\" content=\"width=\(Int(PageLayout.defaultWidth))\">\n" : ""
        let head = """
            <meta http-equiv="Content-Security-Policy" content="\(policy)">
            <meta name="color-scheme" content="dark">
            \(viewport)<style>:root { color-scheme: dark; }</style>

            """
        let page = html.drop(while: \.isWhitespace)
        if page.prefix(9).lowercased() == "<!doctype", let end = page.firstIndex(of: ">") {
            return String(page[...end]) + "\n" + head + String(page[page.index(after: end)...])
        }
        return head + String(page)
    }
}

/// Runs an agent's page sandboxed: no storage that outlives it, a policy that blocks every
/// network load, and only the page itself may load in it. A link the user follows opens in the
/// browser; popups, dialogs and any other navigation do nothing.
///
/// The page lays out at its natural width ([`PageLayout`]) and shows at fit width, or as zoomed
/// as `request` asks, scrolling both ways once wider than the view. A thumbnail always fits and
/// reports its height in points, to be shown whole up to a cap.
@MainActor
struct SandboxedWebView {
    let html: String
    var thumbnail = false
    var request = ZoomRequest()
    /// The scale it shows at, as it changes, by the toolbar or a pinch.
    var scaled: (CGFloat) -> Void = { _ in }
    /// The page's height in points at its scale, as it lays out and on every resize.
    var measured: (CGFloat) -> Void = { _ in }

    /// Lays the page out again whenever its own size changes, so a fitted page stays fitted.
    final class PageView: WKWebView {
        var laidOut: () -> Void = {}
        #if os(macOS)
        override func layout() {
            super.layout()
            laidOut()
        }
        #else
        override func layoutSubviews() {
            super.layoutSubviews()
            laidOut()
        }
        #endif
    }

    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate, WKUIDelegate, WKScriptMessageHandler {
        var html = ""
        var declared: PageLayout.Declared?
        var scaled: (CGFloat) -> Void = { _ in }
        var measured: (CGFloat) -> Void = { _ in }
        var open: (URL) -> Void = { _ in }
        weak var web: PageView?
        /// The page's scroll size in CSS pixels, as it last measured.
        var content = CGSize.zero
        var zoom = Zoom.fit
        var request = -1
        private var observation: NSKeyValueObservation?
        /// The load of `html` has been let through; nothing else will be.
        private var started = false

        func load(_ html: String, in web: PageView) {
            self.html = html
            declared = PageLayout.declared(html)
            content = .zero
            started = false
            web.loadHTMLString(SandboxedPage.wrapped(html), baseURL: nil)
        }

        /// The scale that fits the page's width in the view.
        var fit: CGFloat {
            guard let web else { return 1 }
            let view = web.bounds.width
            return PageLayout.fit(view: view, width: PageLayout.width(declared, view: view, content: content.width))
        }

        func observe(_ web: PageView) {
            self.web = web
            web.laidOut = { [weak self] in self?.layOut() }
            #if os(macOS)
            observation = web.observe(\.magnification) { [weak self] web, _ in
                MainActor.assumeIsolated { self?.magnified(web) }
            }
            #else
            // The scroll view's zoom scale is not observable: its pinch reports it instead.
            web.scrollView.pinchGestureRecognizer?.addTarget(self, action: #selector(pinched))
            #endif
        }

        /// Zooms as the toolbar asks.
        func zoom(to zoom: Zoom) {
            self.zoom = zoom
            #if os(macOS)
            layOut()
            #else
            guard let web else { return }
            // Zoomed about the top-left corner in view, where reading starts, not the centre.
            let scroll = web.scrollView
            let (old, new) = (scroll.zoomScale, zoom.scale(fit: fit))
            let inset = scroll.adjustedContentInset
            let corner = CGPoint(x: scroll.contentOffset.x + inset.left, y: scroll.contentOffset.y + inset.top)
            scroll.setZoomScale(new, animated: false)
            scroll.setContentOffset(CGPoint(x: corner.x * new / old - inset.left, y: corner.y * new / old - inset.top),
                                    animated: false)
            scaled(scroll.zoomScale)
            #endif
        }

        /// Fits the page again to the view's width and, on the Mac, keeps its zoom. The Mac lays
        /// the page out at its natural width with its page zoom, which fits it, then magnifies
        /// that for any zoom beyond: magnifying scrolls, where a larger page zoom would lay the
        /// page out narrower. iOS fits and zooms by its viewport and scroll view.
        func layOut() {
            guard let web, web.bounds.width > 0 else { return }
            let fit = fit
            #if os(macOS)
            let magnification = zoom.scale(fit: fit)
            if abs(web.magnification - magnification) > 0.001 {
                web.setMagnification(magnification, centeredAt: CGPoint(x: web.bounds.midX, y: web.bounds.midY))
            }
            scaled(magnification)
            #else
            // Fitted, WebKit settles its scale after a viewport change without a word: the fit
            // is what shows.
            scaled(zoom == .fit ? fit : web.scrollView.zoomScale)
            #endif
            if content.height > 0 { measured(content.height * fit) }
        }

        #if os(iOS)
        @objc private func pinched() {
            if let web { scaled(web.scrollView.zoomScale) }
        }
        #endif

        #if os(macOS)
        /// A pinch magnified the page: it stays at that scale as the window resizes.
        private func magnified(_ web: WKWebView) {
            let scale = web.magnification
            if abs(scale - zoom.scale(fit: fit)) > 0.001 { zoom = .scale(scale) }
            scaled(scale)
        }
        #endif

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
            guard let size = message.body as? [String: Double], let width = size["width"], let height = size["height"],
                  width.isFinite, height.isFinite, width > 0, height > 0 else { return }
            let measured = CGSize(width: width, height: height)
            guard measured != content else { return }
            content = measured
            #if os(iOS)
            // iOS fits the viewport, not what overflows it: a page wider than the width herder
            // gave it gets a viewport as wide, then fits whole.
            if declared == nil, measured.width > PageLayout.defaultWidth + 0.5, let web {
                let script = "document.getElementById('\(SandboxedPage.viewportId)')?.setAttribute('content', 'width=\(Int(measured.width.rounded(.up)))')"
                web.evaluateJavaScript(script, in: nil, in: .defaultClient) { [weak self] _ in
                    guard let self, zoom == .fit else { return }
                    web.scrollView.setZoomScale(fit, animated: false)
                    layOut()
                }
            }
            #endif
            layOut()
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    /// Posts the page's scroll size from the client world, where the page can't reach the handler.
    private static let measure = """
        (() => {
          const post = () => {
            const root = document.documentElement;
            window.webkit.messageHandlers.size.postMessage({
              width: root.scrollWidth,
              height: Math.ceil(Math.max(root.getBoundingClientRect().height, root.scrollHeight)),
            });
          };
          const observer = new ResizeObserver(post);
          observer.observe(document.documentElement);
          if (document.body) observer.observe(document.body);
          addEventListener("load", post);
          post();
        })();
        """

    private func makeWebView(_ coordinator: Coordinator) -> PageView {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        config.preferences.javaScriptCanOpenWindowsAutomatically = false
        let scripts = config.userContentController
        scripts.add(coordinator, contentWorld: .defaultClient, name: "size")
        scripts.addUserScript(WKUserScript(source: Self.measure, injectionTime: .atDocumentEnd, forMainFrameOnly: true,
                                           in: .defaultClient))
        let web = PageView(frame: .zero, configuration: config)
        web.navigationDelegate = coordinator
        web.uiDelegate = coordinator
        // Past the page's end, zoomed out below it: the app's background, not WebKit's white.
        web.underPageBackgroundColor = .init(Theme.canvas)
        #if os(iOS)
        web.overrideUserInterfaceStyle = .dark
        web.isOpaque = false
        web.backgroundColor = .clear
        web.scrollView.backgroundColor = .clear
        web.scrollView.isScrollEnabled = !thumbnail
        #else
        web.appearance = NSAppearance(named: .darkAqua)
        // No public API for a transparent WKWebView on the Mac; this key is the standard one.
        web.setValue(false, forKey: "drawsBackground")
        web.allowsMagnification = !thumbnail
        #endif
        coordinator.observe(web)
        return web
    }

    private func update(_ web: PageView, _ coordinator: Coordinator, _ openURL: OpenURLAction) {
        coordinator.scaled = scaled
        coordinator.measured = measured
        coordinator.open = { openURL($0) }
        if coordinator.html != html { coordinator.load(html, in: web) }
        if coordinator.request != request.id {
            coordinator.request = request.id
            coordinator.zoom(to: request.zoom)
        }
    }
}

#if os(iOS)
extension SandboxedWebView: UIViewRepresentable {
    func makeUIView(context: Context) -> PageView { makeWebView(context.coordinator) }
    func updateUIView(_ web: PageView, context: Context) { update(web, context.coordinator, context.environment.openURL) }
}
#else
extension SandboxedWebView: NSViewRepresentable {
    func makeNSView(context: Context) -> PageView { makeWebView(context.coordinator) }
    func updateNSView(_ web: PageView, context: Context) { update(web, context.coordinator, context.environment.openURL) }
}
#endif
