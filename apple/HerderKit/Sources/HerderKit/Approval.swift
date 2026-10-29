import Foundation
import Herder
import LocalAuthentication
import Observation

/// Something that asks you to sign with your passkey: a new machine that wants you as its owner
/// (`herder install`'s `herder://own` link), or a terminal signing in as you (`herder login`'s
/// `herder://login` link). Its code must match the one the terminal shows, so nobody else's
/// request is approved in its place.
struct ApprovalRequest: Equatable, Hashable {
    enum Kind: Equatable, Hashable {
        case machine
        case login
    }

    let kind: Kind
    /// The new machine's name, or the machine the terminal runs on; its address until the
    /// machine says its name.
    let name: String
    /// The code the terminal shows: "K7Q4PD".
    let code: String
    /// The machine's TLS fingerprint, hex.
    let fingerprint: String
    /// Where the machine listens: "10.0.4.17:7447".
    let hosts: [String]
    /// The machine's OS and herder version, once it says them.
    var os: String?
    var version: String?

    /// `herder://own?host=<host:port>&fp=<sha256 hex>&code=<code>`, or the same with `login`,
    /// `host` once per address; nil for another link, or one missing what it needs.
    init?(url: URL) {
        guard url.scheme?.lowercased() == "herder",
              let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems
        else { return nil }
        let kind: Kind
        switch url.host()?.lowercased() {
        case "own": kind = .machine
        case "login": kind = .login
        default: return nil
        }
        func value(_ name: String) -> String? {
            items.first { $0.name == name }?.value.flatMap { $0.isEmpty ? nil : $0 }
        }
        let hosts = items.filter { $0.name == "host" }.compactMap(\.value).filter { !$0.isEmpty }
        guard let fingerprint = value("fp"), let code = value("code"), !hosts.isEmpty else { return nil }
        self.init(kind: kind, name: Self.address(hosts[0]), code: code, fingerprint: fingerprint, hosts: hosts)
    }

    init(kind: Kind, name: String, code: String, fingerprint: String, hosts: [String], os: String? = nil,
         version: String? = nil) {
        self.kind = kind
        self.name = name
        self.code = code
        self.fingerprint = fingerprint
        self.hosts = hosts
        self.os = os
        self.version = version
    }

    /// "Linux · 10.0.4.17 · herder 0.17.0": what the machine's link says of it.
    var about: String {
        [os, hosts.first.map(Self.address), version.map { "herder \($0)" }].compactMap { $0 }.joined(separator: " · ")
    }

    /// "K7Q · 4PD": the code in two halves, as the terminal prints it.
    var spokenCode: String { Self.halves(code) }

    nonisolated static func halves(_ code: String) -> String {
        let code = code.uppercased().filter { $0.isLetter || $0.isNumber }
        let middle = code.index(code.startIndex, offsetBy: (code.count + 1) / 2)
        return code.count < 4 ? code : "\(code[..<middle]) · \(code[middle...])"
    }

    /// "ab12 9f3c … e07d": enough of the fingerprint to compare at a glance.
    var shortFingerprint: String { Self.short(fingerprint) }

    nonisolated static func short(_ fingerprint: String) -> String {
        let hex = fingerprint.lowercased().filter(\.isHexDigit)
        guard hex.count > 12 else { return hex }
        return "\(hex.prefix(4)) \(hex.dropFirst(4).prefix(4)) … \(hex.suffix(4))"
    }

    /// "10.0.4.17" of "10.0.4.17:7447", and "fe80::1" of "[fe80::1]:7447".
    nonisolated static func address(_ host: String) -> String {
        if host.hasPrefix("["), let end = host.firstIndex(of: "]") {
            return String(host[host.index(after: host.startIndex)..<end])
        }
        return host.split(separator: ":").count == 2 ? String(host.split(separator: ":")[0]) : host
    }
}

/// Approving a request: the machine gives a challenge, Face ID signs it, then the signature goes
/// back. "Not mine" declines it, and closing the Face ID prompt leaves it to be asked again.
@MainActor @Observable
final class Approval {
    enum State: Equatable {
        case asking
        case signing
        case approved
        case declined
        case failed(String)
    }

    typealias Challenge = @MainActor () async throws -> PasskeyChallenge
    typealias Send = @MainActor (PasskeyAssertion) async throws -> Void

    let request: ApprovalRequest
    private(set) var state = State.asking
    private let authenticator: Authenticator
    private let challenge: Challenge
    private let send: Send

    init(_ request: ApprovalRequest, authenticator: Authenticator, challenge: @escaping Challenge,
         send: @escaping Send) {
        self.request = request
        self.authenticator = authenticator
        self.challenge = challenge
        self.send = send
    }

    func approve() async {
        state = .signing
        do {
            let assertion = try await authenticator.sign(try await challenge())
            try await send(assertion)
            state = .approved
        } catch {
            state = isCancelled(error) ? .asking : .failed(describe(error))
        }
    }

    func decline() {
        state = .declined
    }
}

/// What unlocks the passkey on this device, as buttons name it: "Face ID", "Touch ID".
enum Biometry {
    @MainActor static let name: String = {
        let context = LAContext()
        _ = context.canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: nil)
        switch context.biometryType {
        case .faceID: return "Face ID"
        case .touchID: return "Touch ID"
        case .opticID: return "Optic ID"
        default:
            #if os(iOS)
            return "Face ID"
            #else
            return "Touch ID"
            #endif
        }
    }()
}

extension Fleet {
    /// The machine of a TLS fingerprint, once paired.
    func machine(fingerprint: String) -> Machine? {
        machines.first { $0.fingerprint.lowercased() == fingerprint.lowercased() }
    }

    /// What the passkey signs to approve `request`: a challenge of the machine it names.
    func passkeyChallenge(for request: ApprovalRequest) async throws -> PasskeyChallenge {
        guard request.kind == .login else {
            throw HerderError.Local(detail: "this herder cannot take a new machine yet")
        }
        guard case .passkeyChallenge(let challenge) = try await client.send(hostId: try hostId(of: request),
                                                                           command: .passkeyChallenge)
        else { throw HerderError.Local(detail: "\(request.name) did not send a challenge") }
        return challenge
    }

    /// Sends the signed approval: the terminal is signed in as you.
    func approve(_ request: ApprovalRequest, assertion: PasskeyAssertion) async throws {
        _ = try await client.send(hostId: try hostId(of: request),
                                  command: .approveLogin(code: request.code, assertion: assertion))
    }

    /// The paired machine a terminal's link names, by its fingerprint.
    private func hostId(of request: ApprovalRequest) throws -> HostId {
        guard let machine = machine(fingerprint: request.fingerprint) else {
            throw HerderError.Local(detail: "this device is not paired with \(request.name)")
        }
        return machine.hostId
    }
}
