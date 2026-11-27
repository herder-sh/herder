import Foundation
import Herder
import SwiftUI

#if os(iOS)
import AVFoundation

/// The camera, filling Add a machine's sheet in its form's place, looking for the QR code
/// `herder claim` or `herder login` prints, or Add a Device shows; hands over the link in it.
struct PairScanner: View {
    let scanned: (PairLink) -> Void
    let close: () -> Void

    var body: some View {
        QRScanner(
            hint: "Point the camera at the QR code herder shows on your other device, or herder claim prints.",
            rejected: "That QR code is not one of herder’s.",
            otherwise: "Paste the link instead.",
            close: close
        ) { payload in
            guard let link = pairLink(in: payload) else { return false }
            scanned(link)
            return true
        }
    }
}

/// The camera, full screen, looking for a QR code `accept` takes: it says whether it took
/// the code's text, and the scanner closes once it does.
struct QRScanner: View {
    /// What to point the camera at.
    let hint: String
    /// What a code `accept` does not take is told.
    let rejected: String
    /// What to do instead without a camera, such as "Paste the link instead."
    let otherwise: String
    let close: () -> Void
    let accept: (String) -> Bool
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
                Text(camera.error ?? camera.hint ?? hint)
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
            await camera.start(rejected: rejected, otherwise: otherwise) { payload in
                guard accept(payload) else { return false }
                close()
                return true
            }
        }
        .onDisappear { camera.stop() }
    }
}

/// A capture session that reads QR codes until one holds what its caller takes.
@MainActor
@Observable
private final class QRCamera: NSObject, AVCaptureMetadataOutputObjectsDelegate {
    private(set) var running = false
    private(set) var error: String?
    /// Set once a code was not taken, to say so.
    private(set) var hint: String?
    @ObservationIgnored nonisolated(unsafe) let session = AVCaptureSession()
    @ObservationIgnored private var found: ((String) -> Bool)?
    @ObservationIgnored private var rejected = ""

    func start(rejected: String, otherwise: String, found: @escaping (String) -> Bool) async {
        guard await AVCaptureDevice.requestAccess(for: .video) else {
            error = "herder needs the camera to scan the code: Settings › herder › Camera. Or \(otherwise.lowercasedFirst)"
            return
        }
        guard let device = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: device),
              session.canAddInput(input)
        else {
            error = "This device has no camera herder can use. \(otherwise)"
            return
        }
        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else {
            error = "The camera cannot read QR codes. \(otherwise)"
            return
        }
        session.addInput(input)
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        output.metadataObjectTypes = [.qr]
        self.rejected = rejected
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
        guard payloads.contains(where: found) else {
            hint = rejected
            return
        }
        self.found = nil
    }
}

private extension String {
    /// The string with its first letter lowercased, to follow "Or".
    var lowercasedFirst: String { prefix(1).lowercased() + dropFirst() }
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
