import Foundation
import PDFKit
import SwiftUI
import UniformTypeIdentifiers

#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// A session's artifact on its own: a large window of its own on the Mac, full screen on iOS.
/// Pages, pictures and PDFs open fitting the width and zoom (⌘+ ⌘− ⌘0 for actual size, ⌘9 to
/// fit); the toolbar opens it in the browser, copies its link, saves or shares it, and steps
/// through the session's other artifacts. Esc closes it.
struct ArtifactViewer: View {
    /// Which artifact a Mac window shows.
    struct Ref: Codable, Hashable {
        let host: String
        let session: String
        let id: UInt64

        init(key: SessionKey, id: UInt64) {
            host = key.hostId
            session = key.sessionId
            self.id = id
        }

        var key: SessionKey { SessionKey(hostId: host, sessionId: session) }
    }

    let fleet: Fleet
    let key: SessionKey
    @State private var current: UInt64
    @State private var request = ZoomRequest()
    @State private var scale: CGFloat = 1
    @State private var file: URL?
    @State private var saving = false
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL

    init(fleet: Fleet, key: SessionKey, start: UInt64) {
        self.fleet = fleet
        self.key = key
        _current = State(initialValue: start)
    }

    private var artifacts: [Artifact] { fleet.artifacts(of: key) }
    private var index: Int? { artifacts.firstIndex { $0.id == current } }
    private var artifact: Artifact? { index.map { artifacts[$0] } }
    private var data: Data? { artifact.flatMap { fleet.attachments[$0.attachment.attachmentId] } }
    private var zooms: Bool { [.page, .image, .pdf].contains(artifact?.kind) }

    var body: some View {
        NavigationStack {
            Group {
                if let artifact, let data {
                    ArtifactContent(artifact: artifact, data: data, request: request, scaled: { scale = $0 },
                                    save: { saving = true })
                        .id(artifact.id)
                } else if artifact != nil {
                    ProgressView().tint(Theme.tertiary)
                } else {
                    ContentUnavailableView("This artifact is gone", systemImage: "doc")
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Theme.background)
            .navigationTitle(artifact?.title ?? "Artifact")
            .navigationSubtitle(artifact?.summary ?? "")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar { toolbar }
        }
        .task(id: current) {
            request = ZoomRequest(zoom: .fit, id: request.id + 1)
            file = nil
            guard let artifact else { return }
            await fleet.fetchAttachment(artifact.attachment.attachmentId, of: key)
            file = data.flatMap { try? artifact.file($0) }
        }
        .fileExporter(isPresented: $saving, document: data.map(ArtifactDocument.init), contentType: contentType,
                      defaultFilename: artifact?.name) { _ in }
        #if os(macOS)
        .background {
            // Esc closes the window, wherever the focus is; the traffic lights stay its buttons.
            Button("Close", action: { dismiss() }).keyboardShortcut(.cancelAction)
                .opacity(0).allowsHitTesting(false).accessibilityHidden(true)
        }
        .background(WindowFrameAutosave(name: "artifact"))
        #endif
        .preferredColorScheme(.dark)
        .accessibilityIdentifier("artifact-viewer")
    }

    private var contentType: UTType {
        artifact.flatMap { UTType(filenameExtension: ($0.name as NSString).pathExtension) } ?? .data
    }

    private func zoom(_ zoom: Zoom) { request = ZoomRequest(zoom: zoom, id: request.id + 1) }

    private func step(_ offset: Int) {
        guard let index, artifacts.indices.contains(index + offset) else { return }
        current = artifacts[index + offset].id
    }

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        #if os(macOS)
        ToolbarItemGroup(placement: .navigation) { stepping }
        ToolbarItemGroup(placement: .primaryAction) {
            if zooms { zooming }
            browserButton
            copyButton
            saveButton
            shareButton
        }
        #else
        ToolbarItem(placement: .topBarLeading) {
            Button("Close", systemImage: "xmark") { dismiss() }.keyboardShortcut(.cancelAction)
        }
        ToolbarItemGroup(placement: .topBarTrailing) {
            browserButton
            shareButton
            Menu("More", systemImage: "ellipsis") {
                copyButton
                saveButton
            }
        }
        ToolbarItemGroup(placement: .bottomBar) {
            if artifacts.count > 1 { previousButton }
            Spacer()
            if zooms { zooming }
            Spacer()
            if artifacts.count > 1 { nextButton }
        }
        #endif
    }

    @ViewBuilder private var stepping: some View {
        if artifacts.count > 1 {
            previousButton
            nextButton
        }
    }

    private var previousButton: some View {
        Button("Previous Artifact", systemImage: "chevron.left") { step(-1) }
            .keyboardShortcut("[", modifiers: .command)
            .disabled(index == 0)
            .help("Previous artifact (⌘[)")
    }

    private var nextButton: some View {
        Button("Next Artifact", systemImage: "chevron.right") { step(1) }
            .keyboardShortcut("]", modifiers: .command)
            .disabled(index.map { $0 + 1 >= artifacts.count } ?? true)
            .help("Next artifact (⌘])")
    }

    @ViewBuilder private var zooming: some View {
        Button("Fit Width", systemImage: "arrow.left.and.right.square") { zoom(.fit) }
            .keyboardShortcut("9", modifiers: .command)
            .help("Fit width (⌘9)")
        Button("Zoom Out", systemImage: "minus.magnifyingglass") { zoom(Zoom.smaller(than: scale)) }
            .keyboardShortcut("-", modifiers: .command)
            .help("Zoom out (⌘−)")
        Button { zoom(.scale(1)) } label: {
            Text("\(Int((scale * 100).rounded()))%").monospacedDigit().frame(minWidth: 44)
        }
        .keyboardShortcut("0", modifiers: .command)
        .help("Actual size (⌘0)")
        .accessibilityLabel("Actual Size")
        Button("Zoom In", systemImage: "plus.magnifyingglass") { zoom(Zoom.larger(than: scale)) }
            .keyboardShortcut("+", modifiers: .command)
            .help("Zoom in (⌘+)")
    }

    /// The public link, or on the Mac a private artifact's own file; iOS opens no local files.
    @ViewBuilder private var browserButton: some View {
        TimelineView(.periodic(from: .now, by: 60)) { context in
            if let link = artifact?.link(at: context.date) {
                Button("Open in Browser", systemImage: "safari") { openURL(link) }
                    .help(link.absoluteString)
            } else if ArtifactBrowser.opensFiles, let artifact, let data {
                Button("Open in Browser", systemImage: "safari") { ArtifactBrowser.open(artifact, data) }
                    .help("Open a copy of the file; this project's artifacts get no public link")
            }
        }
    }

    private var copyButton: some View {
        TimelineView(.periodic(from: .now, by: 60)) { context in
            let link = artifact?.link(at: context.date)
            Button("Copy Link", systemImage: "link") { Clipboard.string = link?.absoluteString }
                .disabled(link == nil)
                .help(link == nil ? (artifact?.url == nil ? "Private: no public link" : "The link expired") : "Copy link")
        }
    }

    private var saveButton: some View {
        Button("Save As…", systemImage: "square.and.arrow.down") { saving = true }
            .keyboardShortcut("s", modifiers: .command)
            .disabled(data == nil)
            .help("Save as (⌘S)")
    }

    /// The public link when there is one, else the file itself.
    @ViewBuilder private var shareButton: some View {
        if let link = artifact?.link(at: .now) {
            ShareLink(item: link, subject: Text(artifact?.title ?? "")) { Label("Share", systemImage: "square.and.arrow.up") }
        } else if let file {
            ShareLink(item: file) { Label("Share", systemImage: "square.and.arrow.up") }
        }
    }
}

/// The artifact rendered as what it is.
private struct ArtifactContent: View {
    let artifact: Artifact
    let data: Data
    let request: ZoomRequest
    let scaled: (CGFloat) -> Void
    let save: () -> Void

    var body: some View {
        switch artifact.kind {
        case .page:
            SandboxedWebView(html: String(decoding: data, as: UTF8.self), request: request, scaled: scaled)
        case .image:
            ZoomableImage(data: data, request: request, scaled: scaled)
        case .pdf:
            PDFPages(data: data, request: request, scaled: scaled)
        case .video:
            ArtifactVideo(artifact: artifact, data: data)
        case .text:
            CodeText(artifact: artifact, data: data)
        case .file:
            ContentUnavailableView {
                Label(artifact.name, systemImage: artifact.symbol)
            } description: {
                Text(artifact.summary)
            } actions: {
                Button("Save As…", action: save).buttonStyle(.borderedProminent)
            }
        }
    }
}

/// A picture at fit, zoomed by the toolbar or a pinch and panned once larger than the view.
private struct ZoomableImage: View {
    let data: Data
    let request: ZoomRequest
    let scaled: (CGFloat) -> Void
    @State private var image: SwiftUI.Image?
    @State private var size = CGSize.zero
    @State private var zoom = Zoom.fit
    @State private var pinch: CGFloat = 1

    var body: some View {
        GeometryReader { geometry in
            let fit = size.width > 0
                ? min(1, geometry.size.width / size.width, geometry.size.height / size.height) : 1
            let scale = zoom.scale(fit: fit) * pinch
            ScrollView([.horizontal, .vertical]) {
                image?.resizable().interpolation(.high)
                    .frame(width: size.width * scale, height: size.height * scale)
                    .frame(minWidth: geometry.size.width, minHeight: geometry.size.height)
            }
            .gesture(MagnifyGesture()
                .onChanged { pinch = $0.magnification }
                .onEnded {
                    zoom = .scale(zoom.scale(fit: fit) * $0.magnification)
                    pinch = 1
                })
            .onChange(of: scale, initial: true) { scaled(scale) }
        }
        .onChange(of: request) { zoom = request.zoom }
        .task(id: data) {
            #if os(macOS)
            guard let picture = NSImage(data: data) else { return }
            image = SwiftUI.Image(nsImage: picture)
            #else
            guard let picture = UIImage(data: data) else { return }
            image = SwiftUI.Image(uiImage: picture)
            #endif
            size = picture.size
        }
    }
}

/// Text or code, monospaced and coloured by its language, scrolling both ways unwrapped.
private struct CodeText: View {
    let artifact: Artifact
    let data: Data

    var body: some View {
        GeometryReader { geometry in
            ScrollView([.horizontal, .vertical]) {
                Text(CodeHighlight.attributed(String(decoding: data.prefix(512 * 1024), as: UTF8.self),
                                              language: artifact.language))
                    .font(.callout.monospaced())
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .fixedSize()
                    .padding(16)
                    .frame(minWidth: geometry.size.width, minHeight: geometry.size.height, alignment: .topLeading)
            }
        }
    }
}

/// A PDF in PDFKit, fitting its width, with its pages' thumbnails beside it on the Mac and
/// under it on iOS.
@MainActor
private struct PDFPages {
    let data: Data
    let request: ZoomRequest
    let scaled: (CGFloat) -> Void

    @MainActor final class Coordinator: NSObject {
        let pdf = PDFView()
        var scaled: (CGFloat) -> Void = { _ in }
        var request = -1

        override init() {
            super.init()
            pdf.autoScales = true
            pdf.displayMode = .singlePageContinuous
            NotificationCenter.default.addObserver(self, selector: #selector(changed), name: .PDFViewScaleChanged, object: pdf)
        }

        @objc private func changed() { scaled(pdf.scaleFactor) }

        func zoom(to zoom: Zoom) {
            switch zoom {
            case .fit: pdf.autoScales = true
            case .scale:
                pdf.autoScales = false
                pdf.scaleFactor = zoom.scale(fit: pdf.scaleFactorForSizeToFit)
            }
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    private func stack(_ coordinator: Coordinator) -> PDFView {
        coordinator.pdf.document = PDFDocument(data: data)
        return coordinator.pdf
    }

    private func update(_ coordinator: Coordinator) {
        coordinator.scaled = scaled
        if coordinator.request != request.id {
            coordinator.request = request.id
            coordinator.zoom(to: request.zoom)
        }
    }
}

#if os(macOS)
extension PDFPages: NSViewRepresentable {
    func makeNSView(context: Context) -> NSStackView {
        let thumbnails = PDFThumbnailView()
        thumbnails.pdfView = stack(context.coordinator)
        thumbnails.thumbnailSize = CGSize(width: 96, height: 128)
        thumbnails.backgroundColor = .clear
        thumbnails.widthAnchor.constraint(equalToConstant: 128).isActive = true
        let view = NSStackView(views: [thumbnails, context.coordinator.pdf])
        view.orientation = .horizontal
        view.spacing = 0
        return view
    }

    func updateNSView(_ view: NSStackView, context: Context) { update(context.coordinator) }
}
#else
extension PDFPages: UIViewRepresentable {
    func makeUIView(context: Context) -> UIStackView {
        let thumbnails = PDFThumbnailView()
        thumbnails.pdfView = stack(context.coordinator)
        thumbnails.layoutMode = .horizontal
        thumbnails.thumbnailSize = CGSize(width: 40, height: 52)
        thumbnails.heightAnchor.constraint(equalToConstant: 64).isActive = true
        let view = UIStackView(arrangedSubviews: [context.coordinator.pdf, thumbnails])
        view.axis = .vertical
        return view
    }

    func updateUIView(_ view: UIStackView, context: Context) { update(context.coordinator) }
}
#endif

/// The bytes the save panel writes.
private struct ArtifactDocument: FileDocument {
    static let readableContentTypes: [UTType] = [.data]
    let data: Data

    init(_ data: Data) { self.data = data }

    init(configuration: ReadConfiguration) throws {
        data = configuration.file.regularFileContents ?? Data()
    }

    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: data)
    }
}

#if os(macOS)
/// Keeps the window's frame across openings, under `name`: the first window to claim the name
/// keeps it while open.
private struct WindowFrameAutosave: NSViewRepresentable {
    let name: String

    final class Probe: NSView {
        var name = ""
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            guard let window else { return }
            window.setFrameUsingName(name)
            window.setFrameAutosaveName(name)
        }
    }

    func makeNSView(context: Context) -> Probe {
        let probe = Probe()
        probe.name = name
        return probe
    }

    func updateNSView(_ probe: Probe, context: Context) {}
}
#endif
