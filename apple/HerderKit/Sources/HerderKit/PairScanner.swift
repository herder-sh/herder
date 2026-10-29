import Foundation
import Herder
import SwiftUI

/// The request Add to Fleet takes, in text from a QR code or the clipboard: the `herder://own`
/// link `herder claim` prints, or the `herder://login` link `herder login` prints; alone, or
/// somewhere in a copy of the command's output. Nil when there is none.
func approvalRequest(in text: String) -> ApprovalRequest? {
    text.split(whereSeparator: \.isWhitespace)
        .lazy
        .map { $0.trimmingCharacters(in: CharacterSet(charactersIn: "'\"<>()`")) }
        .compactMap { URL(string: $0).flatMap(ApprovalRequest.init(url:)) }
        .first
}

#if os(iOS)
import AVFoundation

/// The camera, filling Add to Fleet's sheet in its form's place, looking for the QR code
/// `herder claim` or `herder login` prints; hands over the request in it.
struct PairScanner: View {
    let scanned: (ApprovalRequest) -> Void
    let close: () -> Void
    @State private var camera = QRCamera()

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            if camera.running {
                CameraPreview(session: camera.session).ignoresSafeArea()
                RoundedRectangle(cornerRadius: 24)
                    .strokeBorder(Theme.text.opacity(0.9), lineWidth: 3)
                    .frame(width: 260, height: 260)
            }
            VStack(spacing: 0) {
                HStack {
                    Text("Scan the QR code").font(.title3.weight(.bold)).foregroundStyle(Theme.text)
                    Spacer()
                    IconButton(symbol: "xmark", help: "Close", action: close)
                }
                .padding(20)
                Spacer()
                Text(camera.error ?? camera.hint)
                    .font(.subheadline)
                    .multilineTextAlignment(.center)
                    .foregroundStyle(camera.error == nil ? Theme.text : Theme.failure)
                    .padding(14)
                    .frame(maxWidth: .infinity)
                    .background(Theme.sheet.opacity(0.9), in: .rect(cornerRadius: Theme.corner))
                    .padding(20)
            }
        }
        .preferredColorScheme(.dark)
        .task {
            await camera.start { request in
                scanned(request)
                close()
            }
        }
        .onDisappear { camera.stop() }
    }
}

/// A capture session that reads QR codes until one holds a request Add to Fleet takes.
@MainActor
@Observable
private final class QRCamera: NSObject, AVCaptureMetadataOutputObjectsDelegate {
    private(set) var running = false
    private(set) var error: String?
    private(set) var hint = "Point the camera at the QR code herder claim or herder login prints."
    @ObservationIgnored nonisolated(unsafe) let session = AVCaptureSession()
    @ObservationIgnored private var found: ((ApprovalRequest) -> Void)?

    func start(found: @escaping (ApprovalRequest) -> Void) async {
        guard await AVCaptureDevice.requestAccess(for: .video) else {
            error = "herder needs the camera to scan the code: Settings › herder › Camera. Or paste the link instead."
            return
        }
        guard let device = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: device),
              session.canAddInput(input)
        else {
            error = "This device has no camera herder can use. Paste the link instead."
            return
        }
        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else {
            error = "The camera cannot read QR codes. Paste the link instead."
            return
        }
        session.addInput(input)
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        output.metadataObjectTypes = [.qr]
        self.found = found
        // `startRunning` blocks until the camera is up: keep it off the main thread.
        await Task.detached { self.session.startRunning() }.value
        running = true
    }

    func stop() {
        found = nil
        Task.detached { self.session.stopRunning() }
    }

    nonisolated func metadataOutput(
        _ output: AVCaptureMetadataOutput, didOutput objects: [AVMetadataObject], from connection: AVCaptureConnection
    ) {
        let payloads = objects.compactMap { ($0 as? AVMetadataMachineReadableCodeObject)?.stringValue }
        // The delegate queue is the main queue.
        MainActor.assumeIsolated { scanned(payloads) }
    }

    private func scanned(_ payloads: [String]) {
        guard let found, !payloads.isEmpty else { return }
        guard let request = payloads.lazy.compactMap(approvalRequest(in:)).first else {
            hint = "That QR code is not a link herder claim or herder login prints."
            return
        }
        self.found = nil
        found(request)
    }
}

/// The camera's live picture.
private struct CameraPreview: UIViewRepresentable {
    let session: AVCaptureSession

    func makeUIView(context: Context) -> PreviewView {
        let view = PreviewView()
        view.preview.session = session
        view.preview.videoGravity = .resizeAspectFill
        return view
    }

    func updateUIView(_ view: PreviewView, context: Context) {}

    final class PreviewView: UIView {
        override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }
        // `layerClass` makes the layer a preview layer.
        var preview: AVCaptureVideoPreviewLayer { layer as! AVCaptureVideoPreviewLayer }
    }
}
#endif
