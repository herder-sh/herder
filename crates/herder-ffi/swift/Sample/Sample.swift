// The Swift bindings against the fake daemon (`examples/fake_daemon.rs`), run as a sidecar:
// connect, pair, create a session, prompt it and stream the turn. Exits non-zero on failure.
//
//     swift run herder-sample <path to fake_daemon>

import Foundation
import Herder

struct Failure: Error, CustomStringConvertible {
    let description: String
}

@main
struct Sample {
    static func main() async throws {
        setvbuf(stdout, nil, _IOLBF, 0)
        guard CommandLine.arguments.count == 2 else {
            throw Failure(description: "usage: herder-sample <path to fake_daemon>")
        }
        let daemon = Process()
        daemon.executableURL = URL(fileURLWithPath: CommandLine.arguments[1])
        let stdin = Pipe()
        let stdout = Pipe()
        daemon.standardInput = stdin
        daemon.standardOutput = stdout
        try daemon.run()
        defer {
            try? stdin.fileHandleForWriting.close()
            daemon.waitUntilExit()
        }
        let lines = try readLines(3, from: stdout.fileHandleForReading)
        try await pairAndStream(link: lines[0], repo: lines[1], account: lines[2])
        print("ok")
    }

    static func pairAndStream(link: String, repo: String, account: String) async throws {
        try check(pairingUriToString(uri: parsePairingUri(link: link)) == link, "the link round-trips")
        let config = FileManager.default.temporaryDirectory
            .appendingPathComponent("herder-swift-\(UUID().uuidString)")
        let client = try Client.open(configDir: config.path, client: "herder-swift-sample/0")
        let results = try await client.pair(link: link)
        guard results.count == 1, case .paired(let machine) = results[0] else {
            throw Failure(description: "expected one paired machine, got \(results)")
        }
        try check(machine.name == "fake-host", "paired with \(machine.name)")
        let host = machine.hostId
        try await client.synced(hostId: host)
        print("paired with \(machine.name)")

        let created = try await client.send(
            hostId: host,
            command: .createSession(
                repo: repo, projectId: nil, branch: nil, accountId: account, provider: nil,
                model: nil, permissionMode: .ask, maxChildren: nil, failoverPin: nil))
        guard case .sessionCreated(let sessionId) = created else {
            throw Failure(description: "expected a session, got \(created)")
        }
        print("created session \(sessionId)")
        let subscription = try client.subscribeSession(hostId: host, sessionId: sessionId)
        let sent = try await client.send(
            hostId: host, command: .sendPrompt(sessionId: sessionId, text: "Say hello.", images: [], files: []))
        try check(sent == .applied, "the prompt was applied")

        var events: [EventBody] = []
        func turnCompleted() -> Bool {
            events.contains { if case .turnCompleted = $0 { true } else { false } }
        }
        while !(events.contains(.sessionStatusChanged(status: .idle, retryAt: nil)) && turnCompleted()) {
            guard let update = await subscription.next() else {
                throw Failure(description: "the subscription ended")
            }
            events += update.events.map(\.body)
            for item in update.streaming {
                if case .assistantMessage(let text) = item.body { print("streaming: \(text)") }
            }
        }
        let answered = events.contains {
            if case .itemAdded(let item) = $0 { item.body == .assistantMessage(text: "Hello, world.") } else { false }
        }
        try check(answered, "the turn answered: \(events)")
    }

    static func check(_ condition: Bool, _ what: @autoclosure () -> String) throws {
        if !condition { throw Failure(description: "failed: \(what())") }
    }

    /// Reads the first `count` lines a process writes.
    static func readLines(_ count: Int, from handle: FileHandle) throws -> [String] {
        var data = Data()
        while data.filter({ $0 == UInt8(ascii: "\n") }).count < count {
            // Blocks until some output, unlike `read(upToCount:)`, which waits for all of it.
            let chunk = handle.availableData
            if chunk.isEmpty { throw Failure(description: "the fake daemon exited early") }
            data += chunk
        }
        return String(decoding: data, as: UTF8.self)
            .split(separator: "\n").prefix(count).map(String.init)
    }
}
