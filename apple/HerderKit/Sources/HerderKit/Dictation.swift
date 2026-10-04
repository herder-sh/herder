import AVFoundation
import Observation
import Speech

/// Speech to text on this device: Apple's speech model, transcribing the microphone live into
/// the composer. It runs whether or not system Dictation is on; the first use per language
/// downloads the model. Whatever stops it from listening shows as `error`.
@MainActor
@Observable
final class Dictation {
    /// What the user has allowed.
    enum Access: Sendable { case granted, microphoneDenied }

    private(set) var listening = false
    /// The speech model for this language is downloading, before the first listen.
    private(set) var downloading = false
    private(set) var error: String?
    @ObservationIgnored private let authorize: @Sendable () async -> Access
    @ObservationIgnored private let engine = AVAudioEngine()
    @ObservationIgnored private var analyzer: SpeechAnalyzer?
    @ObservationIgnored private var audio: AsyncStream<AnalyzerInput>.Continuation?
    @ObservationIgnored private var results: Task<Void, Never>?

    init(authorize: @escaping @Sendable () async -> Access = Dictation.systemAccess) {
        self.authorize = authorize
    }

    /// Starts listening; `heard` gets the whole transcript so far, each time it changes.
    /// `stop()` while it is still getting ready cancels the start.
    func start(heard: @escaping @MainActor (String) -> Void) async {
        guard !listening else { return }
        listening = true
        error = nil
        let access = await authorize()
        guard listening else { return }
        guard access == .granted else {
            return fail("Allow herder the Microphone in System Settings › Privacy & Security.")
        }
        guard SpeechTranscriber.isAvailable,
              let locale = await SpeechTranscriber.supportedLocale(equivalentTo: .current) else {
            return fail("Speech recognition is not available for this language.")
        }
        let transcriber = SpeechTranscriber(locale: locale, preset: .progressiveTranscription)
        do {
            if let install = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
                downloading = true
                defer { downloading = false }
                try await install.downloadAndInstall()
            }
        } catch {
            return fail("The speech model did not download: \(error.localizedDescription)")
        }
        let input = engine.inputNode
        let microphone = input.outputFormat(forBus: 0)
        guard listening else { return }
        guard let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber], considering: microphone),
              let converter = AVAudioConverter(from: microphone, to: format) else {
            return fail("The microphone's audio cannot be transcribed.")
        }
        let analyzer = SpeechAnalyzer(modules: [transcriber])
        let (stream, audio) = AsyncStream.makeStream(of: AnalyzerInput.self)
        do {
            try await analyzer.start(inputSequence: stream)
        } catch {
            return fail("Dictation did not start: \(error.localizedDescription)")
        }
        guard listening else {
            await analyzer.cancelAndFinishNow()
            return
        }
        // The tap runs on the audio thread, so it is `@Sendable`, not the main actor's; only it
        // uses the converter.
        nonisolated(unsafe) let convert = converter
        input.installTap(onBus: 0, bufferSize: 1024, format: microphone) { @Sendable buffer, _ in
            if let converted = Self.convert(buffer, with: convert) { audio.yield(AnalyzerInput(buffer: converted)) }
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            await analyzer.cancelAndFinishNow()
            return fail("The microphone did not start: \(error.localizedDescription)")
        }
        self.analyzer = analyzer
        self.audio = audio
        // A result is final text, or a guess at what follows it that the next result replaces.
        results = Task { [weak self] in
            var final = ""
            do {
                for try await result in transcriber.results {
                    let text = String(result.text.characters)
                    if result.isFinal { final += text }
                    guard !Task.isCancelled else { return }
                    heard(result.isFinal ? final : final + text)
                }
            } catch {
                guard let self, self.listening else { return }
                self.error = "Dictation stopped: \(error.localizedDescription)"
                self.stop()
            }
        }
    }

    /// Stops listening; the last words still land in `heard`.
    func stop() {
        end { try? await $0.finalizeAndFinishThroughEndOfInput() }
    }

    /// Stops listening and drops the words not yet heard, for when the prompt has gone.
    func cancel() {
        results?.cancel()
        end { await $0.cancelAndFinishNow() }
    }

    private func end(_ finish: @escaping @Sendable (SpeechAnalyzer) async -> Void) {
        guard listening else { return }
        listening = false
        // Not started yet: `start` sees `listening` gone and backs out.
        guard let analyzer else { return }
        engine.stop()
        engine.inputNode.removeTap(onBus: 0)
        audio?.finish()
        Task { await finish(analyzer) }
        self.analyzer = nil
        audio = nil
        results = nil
    }

    private func fail(_ problem: String) {
        error = problem
        listening = false
    }

    /// The microphone's buffer in the format the speech model takes.
    private nonisolated static func convert(_ buffer: AVAudioPCMBuffer, with converter: AVAudioConverter) -> AVAudioPCMBuffer? {
        let ratio = converter.outputFormat.sampleRate / converter.inputFormat.sampleRate
        let capacity = AVAudioFrameCount((Double(buffer.frameLength) * ratio).rounded(.up))
        guard let converted = AVAudioPCMBuffer(pcmFormat: converter.outputFormat, frameCapacity: capacity) else { return nil }
        var given = false
        var failure: NSError?
        converter.convert(to: converted, error: &failure) { _, status in
            if given {
                status.pointee = .noDataNow
                return nil
            }
            given = true
            status.pointee = .haveData
            return buffer
        }
        return failure == nil ? converted : nil
    }

    /// Asks for the microphone. Off the main actor: the system answers on a queue of its own.
    nonisolated static func systemAccess() async -> Access {
        await AVCaptureDevice.requestAccess(for: .audio) ? .granted : .microphoneDenied
    }
}
