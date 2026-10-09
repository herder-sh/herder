import Foundation
import Herder
import Observation

/// What was typed and not sent yet, kept on this device per project for a new session and
/// per session for an open one, so going elsewhere or quitting the app loses none of it.
@MainActor @Observable
final class PromptDrafts {
    struct Content: Codable, Equatable {
        struct Picture: Codable, Equatable {
            let mediaType: String
            let data: Data
        }

        struct Document: Codable, Equatable {
            let name: String
            let data: Data
        }

        var text = ""
        var images: [Picture] = []
        var files: [Document] = []
        /// The new session it starts, for a draft that is not a session's.
        var draft: Draft?

        init(text: String, images: [Herder.Image], files: [PromptFile] = [], draft: Draft? = nil) {
            self.text = text
            self.images = images.map { Picture(mediaType: $0.mediaType, data: $0.data) }
            self.files = files.map { Document(name: $0.name, data: $0.data) }
            self.draft = draft
        }

        var herderImages: [Herder.Image] { images.map { Herder.Image(mediaType: $0.mediaType, data: $0.data) } }
        var promptFiles: [PromptFile] { files.map { PromptFile(name: $0.name, data: $0.data) } }
        var isEmpty: Bool {
            text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && images.isEmpty && files.isEmpty
        }
    }

    /// The folder the drafts are files in, one per project or session.
    @ObservationIgnored let directory: URL
    /// The new sessions written and not started, the last written first.
    private(set) var unsent: [Content] = []
    /// Drafts dropped while open, so leaving them does not keep them again; opening one anew
    /// takes it back.
    @ObservationIgnored private var dropped: Set<String> = []

    init(directory: URL) {
        self.directory = directory
        let written = URLResourceKey.contentModificationDateKey
        let files = (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: [written])) ?? []
        unsent = files
            .compactMap { file -> (content: Content, date: Date)? in
                guard let content = Self.read(file), content.draft != nil else { return nil }
                return (content, (try? file.resourceValues(forKeys: [written]))?.contentModificationDate ?? .distantPast)
            }
            .sorted { $0.date > $1.date }
            .map(\.content)
    }

    static let shared = PromptDrafts(directory: FileManager.default
        .urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("herder/drafts", isDirectory: true))

    /// What a session's draft is kept by; apart from any project's or repository's.
    static func key(_ session: SessionKey) -> String { "session:\(session.hostId)/\(session.sessionId)" }

    /// The draft kept by `key`: a new session's project or repository, or a session's.
    func load(_ key: String) -> Content? {
        dropped.remove(key)
        return Self.read(file(key))
    }

    /// Keeps the draft, or forgets it once it is empty.
    func save(_ content: Content, for key: String) {
        guard !dropped.contains(key) else { return }
        unsent.removeAll { $0.draft?.key == key }
        if !content.isEmpty, content.draft != nil { unsent.insert(content, at: 0) }
        guard !content.isEmpty else {
            try? FileManager.default.removeItem(at: file(key))
            return
        }
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try? JSONEncoder().encode(content).write(to: file(key), options: .atomic)
    }

    /// Moves `content` from the draft `from` to the draft `to`, as the prompt goes along when a
    /// draft moves to another project: `to` keeps its own draft when there is nothing to take.
    func move(_ content: Content, from: String, to: String) {
        guard from != to else { return }
        if !content.isEmpty { save(content, for: to) }
        save(Content(text: "", images: []), for: from)
    }

    /// Forgets a new session's draft, open or not.
    func drop(_ draft: Draft) {
        save(Content(text: "", images: []), for: draft.key)
        dropped.insert(draft.key)
    }

    private static func read(_ file: URL) -> Content? {
        (try? Data(contentsOf: file)).flatMap { try? JSONDecoder().decode(Content.self, from: $0) }
    }

    private func file(_ key: String) -> URL {
        let name = key.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? key
        return directory.appendingPathComponent(name + ".json")
    }
}
