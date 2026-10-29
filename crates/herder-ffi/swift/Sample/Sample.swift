// The Swift bindings against the fake daemon (`examples/fake_daemon.rs`), run as a sidecar:
// sign in with the owner's test passkey, create a session, prompt it and stream the turn. Exits
// non-zero on failure.
//
//     swift run herder-sample <path to fake_daemon>

import CryptoKit
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
        let lines = try readLines(4, from: stdout.fileHandleForReading)
        let passkey = try TestPasskey(lines[1])
        try await signInAndStream(link: lines[0], passkey: passkey, repo: lines[2], account: lines[3])
        print("ok")
    }

    static func signInAndStream(link: String, passkey: TestPasskey, repo: String, account: String) async throws {
        let fleet = try parseFleetLink(link: link)
        try check(fleetLinkToString(link: fleet) == link, "the link round-trips")
        let config = FileManager.default.temporaryDirectory
            .appendingPathComponent("herder-swift-\(UUID().uuidString)")
        let client = try Client.open(configDir: config.path, client: "herder-swift-sample/0")
        let at = fleet.machines[0]
        let pending = try await client.signIn(hosts: at.hosts, fingerprint: at.fingerprint)
        let machine = try await pending.finish(signIn: .passkey(assertion: try passkey.assert(pending.challenge())))
        try check(machine.name == "fake-host", "signed in to \(machine.name)")
        let host = machine.hostId
        try await client.synced(hostId: host)
        print("signed in to \(machine.name)")

        let created = try await client.send(
            hostId: host,
            command: .createSession(
                repo: repo, projectId: nil, chat: false, branch: nil, accountId: account, provider: nil,
                model: nil, permissionMode: .ask, failoverPin: nil))
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

/// The recorded test passkey the fake daemon prints, `<credential id>.<PKCS#8 key>.<user handle>`
/// in base64url:
/// it signs a challenge as a platform authenticator would, with no Face ID to ask.
struct TestPasskey {
    let credentialId: String
    let key: P256.Signing.PrivateKey
    let userHandle: String

    init(_ exported: String) throws {
        let parts = exported.split(separator: ".").map(String.init)
        guard parts.count == 3, let der = Data(base64URL: parts[1]) else {
            throw Failure(description: "not a test passkey: \(exported)")
        }
        credentialId = parts[0]
        userHandle = parts[2]
        key = try P256.Signing.PrivateKey(derRepresentation: der)
    }

    func assert(_ challenge: PasskeyChallenge) throws -> PasskeyAssertion {
        let clientData = try JSONSerialization.data(withJSONObject: [
            "type": "webauthn.get", "challenge": challenge.challenge, "origin": "https://herder.sh",
            "crossOrigin": false,
        ] as [String: Any])
        // The RP id hash, then user present and verified, backup eligible and backed up, and a
        // zero signature counter, as a synced passkey has them.
        var authData = Data(SHA256.hash(data: Data(challenge.rpId.utf8)))
        authData.append(contentsOf: [0x1D, 0, 0, 0, 0])
        let signature = try key.signature(for: authData + Data(SHA256.hash(data: clientData)))
        return PasskeyAssertion(
            id: credentialId, rawId: credentialId,
            response: AssertionResponse(
                clientDataJson: clientData.base64URL, authenticatorData: authData.base64URL,
                signature: signature.derRepresentation.base64URL, userHandle: userHandle),
            kind: "public-key")
    }
}

extension Data {
    init?(base64URL text: String) {
        var base64 = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        base64 += String(repeating: "=", count: (4 - base64.count % 4) % 4)
        self.init(base64Encoded: base64)
    }

    var base64URL: String {
        base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
