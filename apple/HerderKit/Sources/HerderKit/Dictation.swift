import AVFoundation
import Observation
import Speech

/// Speech to text on this device: Apple's speech recognizer, kept on-device so no audio leaves
/// the Mac, transcribing the microphone live into the composer.
@MainActor
@Observable
final class Dictation {
    private(set) var listening = false
    private(set) var error: String?
    @ObservationIgnored private let engine = AVAudioEngine()
    @ObservationIgnored private var request: SFSpeechAudioBufferRecognitionRequest?
    @ObservationIgnored private var task: SFSpeechRecognitionTask?

    /// Starts listening; `heard` gets the whole transcript so far, each time it grows.
    func start(heard: @escaping @MainActor (String) -> Void) async {
        guard !listening else { return }
        guard await Self.authorize() else {
            error = "herder needs microphone and speech recognition access: System Settings › Privacy & Security."
            return
        }
        guard let recognizer = SFSpeechRecognizer(), recognizer.isAvailable else {
            error = "Speech recognition is not available for this language."
            return
        }
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        if recognizer.supportsOnDeviceRecognition { request.requiresOnDeviceRecognition = true }
        let input = engine.inputNode
        // The tap runs on the audio thread; the request only takes buffers from it.
        nonisolated(unsafe) let buffers = request
        input.installTap(onBus: 0, bufferSize: 1024, format: input.outputFormat(forBus: 0)) { buffer, _ in
            buffers.append(buffer)
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            self.error = "The microphone did not start: \(error.localizedDescription)"
            return
        }
        self.request = request
        error = nil
        listening = true
        task = recognizer.recognitionTask(with: request) { [weak self] result, failure in
            let text = result?.bestTranscription.formattedString
            let done = (result?.isFinal ?? false) || failure != nil
            Task { @MainActor in
                if let text { heard(text) }
                if done { self?.stop() }
            }
        }
    }

    func stop() {
        guard listening else { return }
        engine.stop()
        engine.inputNode.removeTap(onBus: 0)
        request?.endAudio()
        task?.finish()
        request = nil
        task = nil
        listening = false
    }

    private static func authorize() async -> Bool {
        let speech = await withCheckedContinuation { continuation in
            SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0 == .authorized) }
        }
        let microphone = await AVCaptureDevice.requestAccess(for: .audio)
        return speech && microphone
    }
}
