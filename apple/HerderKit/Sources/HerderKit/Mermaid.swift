import SwiftUI
import WebKit

#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// Turns Mermaid source into SVG with the bundled `mermaid` library (Resources/Mermaid, pinned
/// to 11.17.2, MIT) in one hidden web view. Results are cached per source, so a diagram that
/// scrolls back into view shows at once instead of rendering again.
@MainActor
final class MermaidRenderer: NSObject, WKNavigationDelegate {
    static let shared = MermaidRenderer()

    enum Outcome: Equatable {
        /// The SVG and its natural size, from its viewBox.
        case diagram(svg: String, size: CGSize)
        /// Mermaid's parse or render error.
        case failed(String)
    }

    private var results: [String: Outcome] = [:]
    private var pending: [String: Task<Outcome, Never>] = [:]
    private var web: WKWebView?
    private var loaded: Bool?
    private var waiting: [CheckedContinuation<Bool, Never>] = []

    func cached(_ source: String) -> Outcome? { results[source] }

    func render(_ source: String) async -> Outcome {
        if let outcome = results[source] { return outcome }
        if let task = pending[source] { return await task.value }
        let task = Task { await run(source) }
        pending[source] = task
        let outcome = await task.value
        pending[source] = nil
        // Views keep the outcome they showed, so dropping the whole cache only costs re-renders.
        if results.count >= 64 { results.removeAll() }
        results[source] = outcome
        return outcome
    }

    private func run(_ source: String) async -> Outcome {
        guard await ready(), let web else { return .failed("Mermaid failed to load.") }
        do {
            let value = try await web.callAsyncJavaScript(
                "return await window.renderDiagram(source)", arguments: ["source": source], contentWorld: .page)
            guard let result = value as? [String: Any] else { return .failed("The diagram could not be rendered.") }
            if let error = result["error"] as? String { return .failed(error) }
            guard let svg = result["svg"] as? String,
                  let width = result["width"] as? Double, let height = result["height"] as? Double,
                  width > 0, height > 0 else { return .failed("The diagram could not be rendered.") }
            return .diagram(svg: svg, size: CGSize(width: width, height: height))
        } catch {
            return .failed(error.localizedDescription)
        }
    }

    /// Loads the renderer page once; false when the bundled library is missing or fails.
    private func ready() async -> Bool {
        if let loaded { return loaded }
        if web == nil { load() }
        return await withCheckedContinuation { waiting.append($0) }
    }

    private func load() {
        guard let url = Bundle.module.url(forResource: "mermaid.min", withExtension: "js", subdirectory: "Mermaid"),
              let library = try? String(contentsOf: url, encoding: .utf8) else {
            finish(false)
            return
        }
        let config = WKWebViewConfiguration()
        let scripts = config.userContentController
        scripts.addUserScript(WKUserScript(source: library, injectionTime: .atDocumentEnd, forMainFrameOnly: true))
        scripts.addUserScript(WKUserScript(source: Self.setup, injectionTime: .atDocumentEnd, forMainFrameOnly: true))
        let web = WKWebView(frame: CGRect(x: 0, y: 0, width: 800, height: 600), configuration: config)
        web.navigationDelegate = self
        self.web = web
        web.loadHTMLString(Self.page, baseURL: nil)
    }

    private func finish(_ ok: Bool) {
        loaded = ok
        let waiting = waiting
        self.waiting = []
        waiting.forEach { $0.resume(returning: ok) }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) { finish(true) }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) { finish(false) }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        finish(false)
    }

    /// The system can kill the page's process; the next render starts a fresh one.
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        web = nil
        loaded = nil
    }

    /// No network: everything the page runs is injected as user scripts.
    private static let page = """
        <!doctype html><meta charset="utf-8">
        <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'">
        <body></body>
        """

    /// Strict security and SVG labels as in T3 Code, with a dark theme built from `Theme`, which
    /// diagram directives can't override.
    private static var setup: String {
        let font = "-apple-system, BlinkMacSystemFont, system-ui, sans-serif"
        let variables: [String: String] = [
            "fontFamily": font, "fontSize": "13px",
            "background": hex(Theme.surface),
            "primaryColor": hex(Theme.raised), "primaryTextColor": hex(Theme.text),
            "primaryBorderColor": "#3A3A3A", "secondaryColor": "#202020", "tertiaryColor": hex(Theme.surface),
            "mainBkg": hex(Theme.raised), "nodeBorder": "#3A3A3A", "textColor": hex(Theme.secondary),
            "lineColor": hex(Theme.tertiary), "clusterBkg": hex(Theme.surface), "clusterBorder": "#2E2E2E",
            "edgeLabelBackground": hex(Theme.surface), "titleColor": hex(Theme.text),
            "actorBkg": hex(Theme.raised), "actorBorder": "#3A3A3A", "actorTextColor": hex(Theme.text),
            "actorLineColor": "#3A3A3A", "signalColor": hex(Theme.secondary), "signalTextColor": hex(Theme.secondary),
            "labelBoxBkgColor": hex(Theme.raised), "labelBoxBorderColor": "#3A3A3A", "labelTextColor": hex(Theme.text),
            "loopTextColor": hex(Theme.secondary), "activationBkgColor": "#242424", "activationBorderColor": "#3A3A3A",
            "noteBkgColor": "#18202E", "noteBorderColor": "#2C3A55", "noteTextColor": hex(Theme.text),
        ]
        let json = (try? JSONSerialization.data(withJSONObject: variables, options: .sortedKeys))
            .flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
        return """
            mermaid.initialize({
              startOnLoad: false, securityLevel: "strict", suppressErrorRendering: true,
              secure: ["secure", "securityLevel", "startOnLoad", "maxTextSize", "suppressErrorRendering",
                       "maxEdges", "htmlLabels", "themeCSS", "theme", "themeVariables", "darkMode", "fontFamily",
                       "fontSize"],
              htmlLabels: false, flowchart: { htmlLabels: false },
              theme: "base", darkMode: true, fontFamily: \(String(reflecting: font)), fontSize: 13, themeVariables: \(json),
            });
            let next = 0;
            window.renderDiagram = async (source) => {
              const id = "mermaid-" + next++;
              try {
                const { svg } = await mermaid.render(id, source);
                const root = new DOMParser().parseFromString(svg, "image/svg+xml").documentElement;
                const box = (root.getAttribute("viewBox") || "").trim().split(/\\s+/).map(Number);
                return { svg, width: box[2], height: box[3] };
              } catch (error) {
                return { error: error instanceof Error ? error.message : String(error) };
              } finally {
                document.getElementById("d" + id)?.remove();
              }
            };
            """
    }

    /// A `Theme` colour as dark-mode `#RRGGBB`.
    private static func hex(_ color: Color) -> String {
        #if os(iOS)
        let resolved = UIColor(color).resolvedColor(with: UITraitCollection(userInterfaceStyle: .dark))
        var (red, green, blue, alpha): (CGFloat, CGFloat, CGFloat, CGFloat) = (0, 0, 0, 0)
        resolved.getRed(&red, green: &green, blue: &blue, alpha: &alpha)
        #else
        var resolved = NSColor.gray
        NSAppearance(named: .darkAqua)?.performAsCurrentDrawingAppearance {
            resolved = NSColor(color).usingColorSpace(.sRGB) ?? .gray
        }
        let (red, green, blue) = (resolved.redComponent, resolved.greenComponent, resolved.blueComponent)
        #endif
        return String(format: "#%02X%02X%02X", Int(red * 255 + 0.5), Int(green * 255 + 0.5), Int(blue * 255 + 0.5))
    }
}

/// A closed ```mermaid fence, as in T3 Code: the diagram in a card, with a toggle to the
/// source, copy, and a larger view to zoom and pan. If Mermaid can't parse it, the error sits
/// above the source.
struct MermaidBlock: View {
    let source: String
    @State private var outcome: MermaidRenderer.Outcome?
    @State private var showCode = false
    @State private var expanded = false
    @State private var copied = false

    init(source: String) {
        self.source = source
        _outcome = State(initialValue: MermaidRenderer.shared.cached(source))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 2) {
                Text("mermaid").font(Theme.monoSmall).foregroundStyle(Theme.tertiary)
                Spacer()
                action(showCode ? "point.3.connected.trianglepath.dotted" : "chevron.left.forwardslash.chevron.right",
                       showCode ? "Show diagram" : "Show code") { showCode.toggle() }
                if case .diagram = outcome, !showCode {
                    action("arrow.up.left.and.arrow.down.right", "Expand diagram") { expanded = true }
                }
                action(copied ? "checkmark" : "doc.on.doc", copied ? "Copied" : "Copy source") {
                    Clipboard.string = source
                    copied = true
                }
            }
            .padding(.leading, 12).padding(.trailing, 6).padding(.top, 6)
            content
        }
        .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
        .task(id: source) { outcome = await MermaidRenderer.shared.render(source) }
        .task(id: copied) {
            guard copied else { return }
            try? await Task.sleep(for: .seconds(1.5))
            copied = false
        }
        .sheet(isPresented: $expanded) {
            if case .diagram(let svg, _) = outcome { MermaidExpanded(source: source, svg: svg) }
        }
    }

    @ViewBuilder private var content: some View {
        switch showCode ? nil : outcome {
        case .diagram(let svg, let size):
            MermaidWebView(svg: svg, interactive: false)
                .aspectRatio(size.width / size.height, contentMode: .fit)
                .frame(maxWidth: size.width)
                .frame(maxWidth: .infinity)
                .padding(12)
                .contentShape(.rect)
                .onTapGesture { expanded = true }
                .accessibilityLabel("Mermaid diagram")
                .accessibilityAddTraits(.isButton)
        case .failed(let message):
            VStack(alignment: .leading, spacing: 0) {
                // Mono, so the caret under Mermaid's parse error lines up.
                Text("Unable to render diagram: \(message)")
                    .font(Theme.monoSmall).foregroundStyle(Theme.failure)
                    .textSelection(.enabled)
                    .padding(.horizontal, 12).padding(.top, 8)
                MarkdownText.codeText(source, language: "mermaid")
            }
        case nil where !showCode:
            Text("Rendering diagram")
                .font(.caption).foregroundStyle(Theme.tertiary)
                .frame(maxWidth: .infinity, minHeight: 144)
        case nil:
            MarkdownText.codeText(source, language: "mermaid")
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

/// The diagram filling a sheet: pinch or ⌘-scroll to zoom, then drag or scroll to pan.
private struct MermaidExpanded: View {
    let source: String
    let svg: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Text("Mermaid diagram").font(.headline).foregroundStyle(Theme.text)
                Spacer()
                IconButton(symbol: "doc.on.doc", help: "Copy source") { Clipboard.string = source }
                IconButton(symbol: "xmark", help: "Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding(16)
            Rectangle().fill(Theme.stroke).frame(height: 1)
            MermaidWebView(svg: svg, interactive: true)
        }
        .background(Theme.surface)
        #if os(macOS)
        .frame(minWidth: 640, idealWidth: 960, minHeight: 440, idealHeight: 680)
        #endif
        .preferredColorScheme(.dark)
    }
}

/// Shows rendered SVG. JavaScript is off and the page can't load anything; the inline one
/// ignores input so the transcript keeps scrolling, the expanded one zooms and pans.
@MainActor
struct MermaidWebView {
    let svg: String
    let interactive: Bool

    final class Coordinator: NSObject, WKNavigationDelegate {
        var svg = ""

        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
            action.navigationType == .other ? .allow : .cancel
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    private func makeWebView(_ coordinator: Coordinator) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.defaultWebpagePreferences.allowsContentJavaScript = false
        let web = interactive ? WKWebView(frame: .zero, configuration: config) : PassiveWebView(frame: .zero, configuration: config)
        web.navigationDelegate = coordinator
        #if os(iOS)
        web.isOpaque = false
        web.backgroundColor = .clear
        web.scrollView.backgroundColor = .clear
        web.scrollView.isScrollEnabled = interactive
        web.isUserInteractionEnabled = interactive
        #else
        // No public API for a transparent WKWebView on the Mac; this key is the standard one.
        web.setValue(false, forKey: "drawsBackground")
        web.allowsMagnification = interactive
        #endif
        return web
    }

    private func update(_ web: WKWebView, _ coordinator: Coordinator) {
        guard coordinator.svg != svg else { return }
        coordinator.svg = svg
        web.loadHTMLString(html, baseURL: nil)
    }

    private var html: String {
        let padding = interactive ? "24px" : "0"
        return """
            <!doctype html><meta charset="utf-8">
            <meta name="viewport" content="width=device-width, initial-scale=1, minimum-scale=1, maximum-scale=8">
            <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:">
            <style>
            html, body { margin: 0; height: 100%; background: transparent; }
            body { box-sizing: border-box; padding: \(padding); }
            svg { display: block; width: 100%; height: 100%; max-width: none !important; }
            </style>
            \(svg)
            """
    }
}

#if os(iOS)
extension MermaidWebView: UIViewRepresentable {
    func makeUIView(context: Context) -> WKWebView { makeWebView(context.coordinator) }
    func updateUIView(_ web: WKWebView, context: Context) { update(web, context.coordinator) }
}

private final class PassiveWebView: WKWebView {}
#else
extension MermaidWebView: NSViewRepresentable {
    func makeNSView(context: Context) -> WKWebView { makeWebView(context.coordinator) }
    func updateNSView(_ web: WKWebView, context: Context) { update(web, context.coordinator) }
}

/// Lets clicks and scrolls fall through to SwiftUI.
private final class PassiveWebView: WKWebView {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}
#endif
