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
    /// The new machine's name, as its link says it, or else its address; for a login, the
    /// machine's address.
    let name: String
    /// The code the terminal shows: "K7Q4PD".
    let code: String
    /// The machine's TLS fingerprint, hex.
    let fingerprint: String
    /// Where the machine listens: "10.0.4.17:7447".
    let hosts: [String]

    /// A `herder://own` link of `herder service install`, or a `herder://login` link of
    /// `herder login`; nil for another link, or one missing what it needs.
    init?(url: URL) {
        let link = url.absoluteString
        switch url.host()?.lowercased() {
        case "own":
            guard let uri = try? parseOwnUri(link: link) else { return nil }
            self.init(kind: .machine, name: uri.name ?? Self.address(uri.hosts.first ?? ""), code: uri.code,
                      fingerprint: uri.fingerprint, hosts: uri.hosts)
        case "login":
            guard let uri = try? parseLoginUri(link: link) else { return nil }
            self.init(kind: .login, name: Self.address(uri.hosts.first ?? ""), code: uri.code,
                      fingerprint: uri.fingerprint, hosts: uri.hosts)
        default:
            return nil
        }
    }

    init(kind: Kind, name: String, code: String, fingerprint: String, hosts: [String]) {
        self.kind = kind
        self.name = name
        self.code = code
        self.fingerprint = fingerprint
        self.hosts = hosts
    }

    /// "10.0.4.17": where the machine is, under its name.
    var about: String { hosts.first.map(Self.address) ?? "" }

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

/// Approving a request: Face ID signs what the machine asks, and the machine takes you as its
/// owner or signs the terminal in. "Not mine" declines it, and closing the Face ID prompt leaves
/// it to be asked again.
@MainActor @Observable
final class Approval {
    enum State: Equatable {
        case asking
        case signing
        case approved
        case declined
        case failed(String)
    }

    /// Approves with the passkey: the machine to open after, for a new one.
    typealias Run = @MainActor (Authenticator) async throws -> HostId?

    let request: ApprovalRequest
    private(set) var state = State.asking
    /// The new machine, once it is yours.
    private(set) var machine: HostId?
    private let authenticator: Authenticator
    private let run: Run

    init(_ request: ApprovalRequest, authenticator: Authenticator, run: @escaping Run) {
        self.request = request
        self.authenticator = authenticator
        self.run = run
    }

    func approve() async {
        state = .signing
        do {
            machine = try await run(authenticator)
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

    /// Approves `request` with your passkey: a new machine takes you as its owner and is saved,
    /// its host id returned; a terminal is signed in as you.
    func approve(_ request: ApprovalRequest, with authenticator: Authenticator) async throws -> HostId? {
        switch request.kind {
        case .machine:
            let pending = try await client.signIn(hosts: request.hosts, fingerprint: request.fingerprint)
            let person = try await owner(pending, authenticator: authenticator)
            return try await pending.finish(signIn: .claimOwnership(code: request.code, person: person)).hostId
        case .login:
            guard let hostId = machine(fingerprint: request.fingerprint)?.hostId else {
                throw HerderError.Local(detail: "this device is not paired with \(request.name)")
            }
            guard case .passkeyChallenge(let challenge, _) = try await client.send(hostId: hostId, command: .passkeyChallenge)
            else { throw HerderError.Local(detail: "\(request.name) did not send a challenge") }
            let assertion = try await authenticator.sign(challenge)
            _ = try await client.send(hostId: hostId, command: .approveLogin(code: request.code, assertion: assertion))
            return nil
        }
    }

    /// Signs this device in to the machine at `hosts` with `authenticator`'s passkey, as the
    /// person the machine knows by it, and saves the machine; its host id returned.
    func signIn(hosts: [String], fingerprint: String, with authenticator: Authenticator) async throws -> HostId {
        let pending = try await client.signIn(hosts: hosts, fingerprint: fingerprint)
        let assertion = try await authenticator.sign(pending.challenge())
        return try await pending.finish(signIn: .passkey(assertion: assertion)).hostId
    }

    /// You, as a new machine's owner: with the passkey your machines know, else with one made now.
    private func owner(_ pending: PendingSignIn, authenticator: Authenticator) async throws -> NewPerson {
        let people = FleetPeople(fleet: self)
        await people.reload(machines)
        let me = FleetPeople.me(people.people, machines: machines)
        if let known = Self.keyed(me, in: people.people) {
            let assertion = try await authenticator.sign(pending.challenge())
            guard let key = known.passkeys.first(where: { $0.credentialId == assertion.id }) else {
                throw HerderError.Local(detail: "that passkey is not the one your machines know")
            }
            return NewPerson(name: known.name, passkey: .existing(personId: known.personId, key: key, assertion: assertion))
        }
        guard let creation = pending.registration(),
              let userId = Data(base64URL: creation.userHandle), let challenge = Data(base64URL: creation.challenge)
        else { throw HerderError.Local(detail: "the machine did not offer to make a passkey") }
        let name = people.people.values.joined().first { $0.personId == me }?.name ?? NSFullUserName()
        let registration = try await authenticator.register(name: name, userId: userId, challenge: challenge)
        return NewPerson(name: name, passkey: .register(registration: registration))
    }

    /// You as a machine lists you with your passkeys, when one does.
    nonisolated static func keyed(_ me: PersonId?, in people: [HostId: [PersonInfo]]) -> PersonInfo? {
        people.values.joined().first { $0.personId == me && !$0.passkeys.isEmpty }
    }
}
