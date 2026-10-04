import AVFoundation
import Observation
import Speech

/// Speech to text on this device: Apple's speech recognizer, kept on-device when the Mac has
/// the model, transcribing the microphone live into the composer. Whatever stops it from
/// listening shows as `error`.
@MainActor
@Observable
final class Dictation {
    /// What the user has allowed.
    enum Access: Sendable { case granted, speechDenied, microphoneDenied }

    private(set) var listening = false
    private(set) var error: String?
    @ObservationIgnored private let authorize: @Sendable () async -> Access
    @ObservationIgnored private let engine = AVAudioEngine()
    @ObservationIgnored private var request: SFSpeechAudioBufferRecognitionRequest?
    @ObservationIgnored private var task: SFSpeechRecognitionTask?

    init(authorize: @escaping @Sendable () async -> Access = Dictation.systemAccess) {
        self.authorize = authorize
    }

    /// Starts listening; `heard` gets the whole transcript so far, each time it grows.
    func start(heard: @escaping @MainActor (String) -> Void) async {
        guard !listening else { return }
        error = nil
        switch await authorize() {
        case .granted: break
        case .speechDenied:
            error = "Allow herder Speech Recognition in System Settings › Privacy & Security."
            return
        case .microphoneDenied:
            error = "Allow herder the Microphone in System Settings › Privacy & Security."
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
        // The tap runs on the audio thread, so it is `@Sendable`, not the main actor's; the
        // request only takes buffers from it.
        nonisolated(unsafe) let buffers = request
        input.installTap(onBus: 0, bufferSize: 1024, format: input.outputFormat(forBus: 0)) { @Sendable buffer, _ in
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
        listening = true
        // Results arrive on the recognizer's queue.
        task = recognizer.recognitionTask(with: request) { @Sendable [weak self] result, failure in
            let text = result?.bestTranscription.formattedString
            let done = (result?.isFinal ?? false) || failure != nil
            let problem = failure.flatMap(Self.message(for:))
            Task { @MainActor in
                guard let self else { return }
                if let text { heard(text) }
                // A failure after the user stopped is the task winding down, not news.
                if let problem, self.listening { self.error = problem }
                if done { self.stop() }
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

    /// What to tell the user when recognition fails; nil when there is nothing to tell.
    nonisolated static func message(for failure: any Error) -> String? {
        let failure = failure as NSError
        switch (failure.domain, failure.code) {
        case ("kLSRErrorDomain", 201), ("kAFAssistantErrorDomain", 1700):
            // The recognizer only runs with Dictation (or Siri) on.
            return "Turn on Dictation in System Settings › Keyboard to dictate."
        case ("kAFAssistantErrorDomain", 1110):
            return "No speech heard."
        case ("kAFAssistantErrorDomain", 216), ("kAFAssistantErrorDomain", 301), ("kLSRErrorDomain", 301):
            // Cancelled: the user stopped.
            return nil
        default:
            return "Dictation stopped: \(failure.localizedDescription)"
        }
    }

    /// Asks for speech recognition, then the microphone. Off the main actor: the system answers
    /// on a queue of its own.
    nonisolated static func systemAccess() async -> Access {
        let speech = await withCheckedContinuation { continuation in
            SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0 == .authorized) }
        }
        guard speech else { return .speechDenied }
        return await AVCaptureDevice.requestAccess(for: .audio) ? .granted : .microphoneDenied
    }
}
