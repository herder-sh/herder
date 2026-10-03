import Herder
import SwiftUI

/// The `herder://pair` link in text from a QR code or the clipboard: the link alone, or a copy
/// of `herder pair`'s output with the link somewhere in it. Nil when there is none.
func pairingLink(in text: String) -> String? {
    text.split(whereSeparator: \.isWhitespace)
        .lazy
        .map { $0.trimmingCharacters(in: CharacterSet(charactersIn: "'\"<>()`")) }
        .first { (try? parsePairingUri(link: $0)) != nil }
}

#if os(iOS)
import AVFoundation

/// The camera, full screen, looking for the QR code `herder pair` prints; hands over the
/// pairing link in it.
struct PairScanner: View {
    let scanned: (String) -> Void
    @Environment(\.dismiss) private var dismiss
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
                    IconButton(symbol: "xmark", help: "Close") { dismiss() }
                }
                .padding(20)
                Spacer()
                Text(camera.error ?? camera.hint)
                    .font(.subheadline)
                    .multilineTextAlignment(.center)
                    .foregroundStyle(camera.error == nil ? Theme.text : Theme.failure)
                    .padding(14)
                    .frame(maxWidth: .infinity)
                    .background(Theme.surface.opacity(0.9), in: .rect(cornerRadius: Theme.corner))
                    .padding(20)
            }
        }
        .preferredColorScheme(.dark)
        .task {
            await camera.start { link in
                scanned(link)
                dismiss()
            }
        }
        .onDisappear { camera.stop() }
    }
}

/// A capture session that reads QR codes until one holds a pairing link.
@MainActor
@Observable
private final class QRCamera: NSObject, AVCaptureMetadataOutputObjectsDelegate {
    private(set) var running = false
    private(set) var error: String?
    private(set) var hint = "Point the camera at the QR code herder pair prints."
    @ObservationIgnored nonisolated(unsafe) let session = AVCaptureSession()
    @ObservationIgnored private var found: ((String) -> Void)?

    func start(found: @escaping (String) -> Void) async {
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
        let session = session
        await Task.detached { session.startRunning() }.value
        running = true
    }

    func stop() {
        found = nil
        let session = session
        Task.detached { session.stopRunning() }
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
        guard let link = payloads.lazy.compactMap(pairingLink(in:)).first else {
            hint = "That QR code is not a herder pairing link."
            return
        }
        self.found = nil
        found(link)
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
