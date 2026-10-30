import Foundation
import Herder
@testable import HerderKit

/// The fake daemon of crates/herder-ffi (`examples/fake_daemon.rs`), run as a sidecar. Its path
/// comes from `HERDER_FAKE_DAEMON`; tests that need it are skipped without it.
final class FakeDaemon {
    static let path = ProcessInfo.processInfo.environment["HERDER_FAKE_DAEMON"]

    /// Its `herder://fleet` link.
    let link: String
    /// Its owner's recorded passkey, as the daemon prints it (`RecordedPasskey`).
    let passkey: String
    let repo: String
    let account: String
    /// The account whose turn runs until it is interrupted (`fixtures/hold.jsonl`).
    let holdAccount: String
    /// A member's recorded passkey, where `passkey` is the owner's.
    let memberPasskey: String
    /// A repository with a skill to import, `changelog`, in a folder of its name.
    let skillSource: String
    /// The account whose turn asks a question (`fixtures/question.jsonl`).
    let questionAccount: String
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
        let lines = try Self.readLines(8, from: stdout.fileHandleForReading)
        (link, passkey, repo, account, holdAccount, memberPasskey, skillSource, questionAccount) = (
            lines[0], lines[1], lines[2], lines[3], lines[4], lines[5], lines[6], lines[7]
        )
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
    /// Signs in to `daemon`, the one machine its link names, as its owner or as a member, with
    /// their recorded passkey.
    @discardableResult
    func signIn(to daemon: FakeDaemon, member: Bool = false) async throws -> Machine {
        let at = try parseFleetLink(link: daemon.link).machines[0]
        guard let passkey = RecordedPasskey(member ? daemon.memberPasskey : daemon.passkey) else {
            throw HerderError.Local(detail: "the fake daemon printed no passkey")
        }
        let hostId = try await signIn(hosts: at.hosts, fingerprint: at.fingerprint, with: passkey)
        update(client.machines())
        guard let machine = client.machines().first(where: { $0.hostId == hostId }) else {
            throw HerderError.UnknownMachine(hostId: hostId)
        }
        return machine
    }
}

/// A fresh profile directory.
func temporaryProfile() -> URL {
    FileManager.default.temporaryDirectory.appendingPathComponent("herder-apple-\(UUID().uuidString)")
}

/// Waits until `condition` holds, checking every 50 ms for up to `seconds`. Every test shares
/// the main actor, and CI's parallel run keeps it busy for many seconds at a time, so the wait
/// is long; a condition that holds still returns at once.
@MainActor
func eventually(within seconds: Double = 30, _ condition: () -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(seconds)
    while ContinuousClock.now < deadline {
        if condition() { return true }
        try? await Task.sleep(for: .milliseconds(50))
    }
    return condition()
}
