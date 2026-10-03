import Foundation
import Herder
import Observation

/// The paired machines, kept current from the client's change notifications.
@MainActor
@Observable
public final class Fleet {
    public let client: Client
    public private(set) var machines: [Machine]

    public init(client: Client) {
        self.client = client
        machines = client.machines()
    }

    /// Follows the client's changes until it stops; runs for as long as the fleet is shown.
    public func follow() async {
        // Subscribe before reading, so a change between `init` and now is not missed.
        let changes = client.changes()
        machines = client.machines()
        while await changes.next() {
            machines = client.machines()
        }
    }

    /// Pairs with the daemon a `herder://pair` link names.
    @discardableResult
    public func pair(link: String) async throws -> Machine {
        let machine = try await client.pair(link: link)
        machines = client.machines()
        return machine
    }

    /// The app is in the foreground: reconnects now and replaces dead connections.
    public func wake() {
        client.wake()
    }

    /// The app went to the background: saves the offline cache and stops retrying.
    public func suspend() {
        client.suspend()
    }
}

/// The profile this device keeps its pairings and offline cache in.
public enum Profile {
    case opened(Fleet)
    case failed(String)

    /// Opens the profile in `directory`, creating it if needed.
    @MainActor
    public static func open(at directory: URL, client name: String) -> Profile {
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            return .opened(Fleet(client: try Client.open(configDir: directory.path, client: name)))
        } catch {
            return .failed(describe(error))
        }
    }

    /// Opens the app's profile in Application Support.
    @MainActor
    public static func openDefault() -> Profile {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        #if os(iOS)
        let platform = "ios"
        #else
        let platform = "macos"
        #endif
        return open(
            at: support.appendingPathComponent("herder", isDirectory: true),
            client: "herder-\(platform)/\(version ?? "dev")")
    }
}
