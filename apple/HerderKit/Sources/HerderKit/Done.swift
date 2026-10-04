import Foundation
import Herder

/// The sessions that finished a turn since this device last opened them: the TUI's *done*
/// (docs/tui-design.md §3.2, crates/herder-tui/src/nav.rs). A session is done once this device
/// sees it go from running to idle while it is not open; opening it, or its next turn, clears
/// it. Kept per device, in the profile's cache, so it survives a restart.
struct DoneSessions {
    private(set) var keys: Set<SessionKey>
    /// Where the set is saved; `nil` keeps it in memory only.
    private let file: URL?

    init(file: URL?) {
        self.file = file
        let saved = file.flatMap { try? Data(contentsOf: $0) }
            .flatMap { try? JSONDecoder().decode([[String]].self, from: $0) } ?? []
        keys = Set(saved.compactMap { pair in
            pair.count == 2 ? SessionKey(hostId: pair[0], sessionId: pair[1]) : nil
        })
    }

    func contains(_ key: SessionKey) -> Bool { keys.contains(key) }

    /// Notes what an update did to `key`'s status. `wasLoaded` is false for the history a new
    /// subscription replays, which is no news; a session open on this device sees its own
    /// turn end.
    mutating func observe(_ key: SessionKey, before: SessionStatus, after: SessionStatus, wasLoaded: Bool, open: Bool) {
        if after != .idle {
            remove(key)
        } else if wasLoaded && before == .running && !open {
            insert(key)
        }
    }

    /// The user opened `key`, or it is no longer listed.
    mutating func remove(_ key: SessionKey) {
        guard keys.remove(key) != nil else { return }
        save()
    }

    private mutating func insert(_ key: SessionKey) {
        guard keys.insert(key).inserted else { return }
        save()
    }

    private func save() {
        guard let file else { return }
        let pairs = keys.map { [$0.hostId, $0.sessionId] }.sorted { $0.lexicographicallyPrecedes($1) }
        guard let data = try? JSONEncoder().encode(pairs) else { return }
        try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: file, options: .atomic)
    }
}

extension SessionState {
    /// Roll-up priority, as the TUI's: a project shows its highest-priority session.
    var priority: Int {
        switch self {
        case .needsYou: 6
        case .error: 5
        case .done: 4
        case .running: 3
        case .waiting: 2
        case .idle: 1
        case .archived, .moved: 0
        }
    }

    /// The state a project shows for its sessions' `states`: the highest priority, the first of
    /// equals; `nil` for none.
    static func rollup(_ states: some Sequence<SessionState>) -> SessionState? {
        states.reduce(nil) { shown, state in
            guard let shown, state.priority <= shown.priority else { return state }
            return shown
        }
    }
}
