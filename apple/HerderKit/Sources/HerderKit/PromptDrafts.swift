import Foundation
import Herder

/// What was typed and not sent yet, kept on this device per project for a new session and
/// per session for an open one, so going elsewhere or quitting the app loses none of it.
struct PromptDrafts {
    struct Content: Codable, Equatable {
        struct Picture: Codable, Equatable {
            let mediaType: String
            let data: Data
        }

        var text = ""
        var images: [Picture] = []

        init(text: String, images: [Herder.Image]) {
            self.text = text
            self.images = images.map { Picture(mediaType: $0.mediaType, data: $0.data) }
        }

        var herderImages: [Herder.Image] { images.map { Herder.Image(mediaType: $0.mediaType, data: $0.data) } }
        var isEmpty: Bool { text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && images.isEmpty }
    }

    /// The folder the drafts are files in, one per project or session.
    let directory: URL

    static let shared = PromptDrafts(directory: FileManager.default
        .urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("herder/drafts", isDirectory: true))

    /// What a session's draft is kept by; apart from any project's or repository's.
    static func key(_ session: SessionKey) -> String { "session:\(session.hostId)/\(session.sessionId)" }

    /// The draft kept by `key`: a new session's project or repository, or a session's.
    func load(_ key: String) -> Content? {
        (try? Data(contentsOf: file(key))).flatMap { try? JSONDecoder().decode(Content.self, from: $0) }
    }

    /// Keeps the draft, or forgets it once it is empty.
    func save(_ content: Content, for key: String) {
        guard !content.isEmpty else {
            try? FileManager.default.removeItem(at: file(key))
            return
        }
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try? JSONEncoder().encode(content).write(to: file(key), options: .atomic)
    }

    private func file(_ key: String) -> URL {
        let name = key.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? key
        return directory.appendingPathComponent(name + ".json")
    }
}
