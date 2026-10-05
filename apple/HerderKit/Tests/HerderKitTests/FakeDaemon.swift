import Foundation
import Herder
@testable import HerderKit

/// The fake daemon of crates/herder-ffi (`examples/fake_daemon.rs`), run as a sidecar. Its path
/// comes from `HERDER_FAKE_DAEMON`; tests that need it are skipped without it.
final class FakeDaemon {
    static let path = ProcessInfo.processInfo.environment["HERDER_FAKE_DAEMON"]

    let link: String
    let repo: String
    let account: String
    /// The account whose turn runs until it is interrupted (`fixtures/hold.jsonl`).
    let holdAccount: String
    /// Pairs a device as a member, where `link` pairs it as the owner.
    let memberLink: String
    private let process = Process()
    private let stdin = Pipe()

    /// A daemon of host `name`.
    init(name: String = "fake-host") throws {
        guard let path = Self.path else { throw CocoaError(.fileNoSuchFile) }
        let stdout = Pipe()
        process.executableURL = URL(fileURLWithPath: path)
        process.arguments = [name]
        process.standardInput = stdin
        process.standardOutput = stdout
        try process.run()
        let lines = try Self.readLines(5, from: stdout.fileHandleForReading)
        (link, repo, account, holdAccount, memberLink) = (lines[0], lines[1], lines[2], lines[3], lines[4])
    }

    deinit {
        // The daemon runs until its stdin closes.
        try? stdin.fileHandleForWriting.close()
        process.waitUntilExit()
    }

    /// Reads the first `count` lines a process writes.
    private static func readLines(_ count: Int, from handle: FileHandle) throws -> [String] {
        var data = Data()
        while data.filter({ $0 == UInt8(ascii: "\n") }).count < count {
            // Blocks until some output, unlike `read(upToCount:)`, which waits for all of it.
            let chunk = handle.availableData
            if chunk.isEmpty { throw CocoaError(.fileReadUnknown) }
            data += chunk
        }
        return String(decoding: data, as: UTF8.self)
            .split(separator: "\n").prefix(count).map(String.init)
    }
}

extension Fleet {
    /// Pairs with `daemon`, the one machine its link names, as its owner or as a member.
    @discardableResult
    func pair(_ daemon: FakeDaemon, member: Bool = false) async throws -> Machine {
        let results = try await pair(link: member ? daemon.memberLink : daemon.link)
        guard case .paired(let machine) = results.first, results.count == 1 else {
            throw HerderError.Pairing(detail: "did not pair: \(results)")
        }
        return machine
    }
}

/// A fresh profile directory.
func temporaryProfile() -> URL {
    FileManager.default.temporaryDirectory.appendingPathComponent("herder-apple-\(UUID().uuidString)")
}

/// Waits until `condition` holds, checking every 50 ms for up to `seconds`.
@MainActor
func eventually(within seconds: Double = 10, _ condition: () -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(seconds)
    while ContinuousClock.now < deadline {
        if condition() { return true }
        try? await Task.sleep(for: .milliseconds(50))
    }
    return condition()
}
