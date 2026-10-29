import Foundation
import Herder
import Observation

/// The one-time move to one identity: on each machine you own, your old user (or users) become
/// your passkey. Machines that show up as users move to the vault's Machines list, and users
/// nobody can place wait for your decision. Offered once on launch while a machine you own is
/// not claimed yet.
struct ClaimPlan: Hashable {
    /// A machine that becomes yours.
    struct Machine: Hashable, Identifiable {
        var id: HostId { hostId }
        let hostId: HostId
        let name: String
        /// The user this device is there now, which becomes you.
        let userId: UserId
        /// Your users there now, by name, when known: "tomas", "owner".
        var users: [String] = []
        /// The users there that are machines, which move to the vault's Machines list.
        var machineRows: [String] = []
    }

    /// A user on a machine that might be you, or someone else.
    struct Question: Hashable, Identifiable {
        var id: String { "\(userId)@\(hostId)" }
        let hostId: HostId
        let userId: UserId
        let user: String
        let machine: String
        /// Why herder cannot tell: "A user from Oct 9 whose CLI device is online."
        let reason: String
    }

    enum Answer: String, CaseIterable, Equatable {
        case me, someoneElse, remove

        var title: String {
            switch self {
            case .me: "It's me"
            case .someoneElse: "Someone else"
            case .remove: "Remove"
            }
        }

        /// What the machine is told; someone else stays as they are, for an invite later.
        var decision: LeftoverDecision? {
            switch self {
            case .me: .itsMe
            case .someoneElse: nil
            case .remove: .remove
            }
        }
    }

    var machines: [Machine]
    var questions: [Question] = []
    /// What the passkey is saved under: your name.
    var name = "Owner"
    /// You with your passkey, when a machine knows it already: it is reused, not made anew.
    var known: PersonInfo?
    /// Where the passkey will be kept.
    var keychain = "iCloud Keychain"
    /// What unlocks it: "Face ID".
    var biometry = "Face ID"
}

extension ClaimPlan.Machine {
    /// "tomas (owner) → you · 3 machine rows move to Machines".
    var change: String {
        let from = "\(users.isEmpty ? "your user" : users.joined(separator: ", ")) (owner) → you"
        switch machineRows.count {
        case 0: return from
        case 1: return "\(from) · \(machineRows[0]) moves to Machines"
        case let count: return "\(from) · \(count) machine rows move to Machines"
        }
    }
}

extension ClaimPlan.Question {
    /// A user `claimed` left on `machine`, with what tells who they are: when they paired, and
    /// whether a device of theirs is online.
    init(leftover: UserInfo, hostId: HostId, machine: String) {
        let since = leftover.devices.first.flatMap { Timestamp.date($0.pairedAt) }
            .map { " from \($0.formatted(.dateTime.month(.abbreviated).day()))" } ?? ""
        let online = leftover.devices.contains(where: \.online) ? " whose device is online" : ""
        self.init(hostId: hostId, userId: leftover.userId, user: leftover.name, machine: machine,
                  reason: "A\(leftover.role == .owner ? "n owner" : " member")\(since)\(online). Is this you?")
    }
}

extension ClaimPlan {
    /// "Create your herder passkey, then claim 5 machines".
    var headline: String {
        let count = machines.count == 1 ? "1 machine" : "\(machines.count) machines"
        return known == nil ? "Create your herder passkey, then claim \(count)" : "Claim \(count) with your herder passkey"
    }

    /// "Stored in iCloud Keychain; Face ID once for all five."
    var detail: String {
        let all = machines.count == 1 ? "it" : machines.count == 2 ? "both" : "all \(Self.word(machines.count))"
        return "Stored in \(keychain); \(biometry) once for \(all)."
    }

    nonisolated static func word(_ number: Int) -> String {
        let words = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"]
        return number < words.count ? words[number] : "\(number)"
    }

    /// The machines you own that still know you by a per-daemon user: connected, owner, and no
    /// person in their hello.
    nonisolated static func unclaimed(_ machines: [Herder.Machine]) -> [Machine] {
        machines.compactMap { machine in
            guard machine.role == .owner, machine.personId == nil, machine.connection == .connected,
                  let userId = machine.userId
            else { return nil }
            return Machine(hostId: machine.hostId, name: machine.name, userId: userId)
        }
    }

    /// The mockup's: five machines, one "owner" nobody can place.
    static let demo = ClaimPlan(
        machines: [
            Machine(hostId: "mac", name: "This Mac", userId: "u-mac", users: ["tomas"]),
            Machine(hostId: "trash", name: "trash-can-01", userId: "u-trash", users: ["tomas"]),
            Machine(hostId: "draugelis", name: "draugelis", userId: "u-draugelis", users: ["tomas"]),
            Machine(hostId: "vault-0", name: "herder-vault-0", userId: "u-vault-0", users: ["tomas", "owner"],
                    machineRows: ["trash-can-01", "Coingate-CJH9FXV367", "draugelis"]),
            Machine(hostId: "draugelis-vault", name: "draugelis vault", userId: "u-draugelis-vault", users: ["tomas"],
                    machineRows: ["draugelis"]),
        ],
        questions: [Question(hostId: "vault-0", userId: "u-owner", user: "owner", machine: "herder-vault-0",
                             reason: "A user from Oct 9 whose CLI device is online. Is this you?")],
        name: "Tomas")
}

extension Fleet {
    /// Claim your machines, for the machines you own that are not claimed yet; nil when none is.
    func claimPlan() async -> ClaimPlan? {
        let machines = ClaimPlan.unclaimed(self.machines)
        guard !machines.isEmpty else { return nil }
        let people = FleetPeople(fleet: self)
        await people.reload(self.machines)
        let me = FleetPeople.me(self.machines)
        let known = Self.keyed(me, in: people.people)
        return ClaimPlan(machines: machines, name: Self.ownerName(known?.name, fullName: NSFullUserName()),
                         known: known, biometry: Biometry.name)
    }
}

/// Claiming: the passkey is made on the first machine (or the one your machines know is used),
/// then each machine takes it with your users on the others in `also`. The users a machine
/// leaves over then wait for your decision.
@MainActor @Observable
final class Claim {
    enum Row: Equatable {
        case ready, claiming, claimed, failed
    }

    typealias Send = @MainActor (HostId, CommandBody) async throws -> CommandResult

    let plan: ClaimPlan
    /// The machines, with the machine rows each moved once claimed.
    private(set) var machines: [ClaimPlan.Machine]
    /// The users waiting for a decision.
    private(set) var questions: [ClaimPlan.Question]
    var answers: [String: ClaimPlan.Answer]
    private(set) var rows: [HostId: Row]
    private(set) var working = false
    private(set) var error: String?
    /// You, once a machine knows your passkey: the person and its public keys.
    private var me: (id: PersonId, keys: [PasskeyKey])?
    private let authenticator: Authenticator
    private let send: Send

    init(_ plan: ClaimPlan, authenticator: Authenticator, send: @escaping Send) {
        self.plan = plan
        self.authenticator = authenticator
        self.send = send
        machines = plan.machines
        questions = plan.questions
        answers = Dictionary(uniqueKeysWithValues: plan.questions.map { ($0.id, .me) })
        rows = Dictionary(uniqueKeysWithValues: plan.machines.map { ($0.id, .ready) })
        me = plan.known.map { ($0.personId, $0.passkeys) }
    }

    /// Every machine is yours.
    var claimed: Bool { rows.values.allSatisfy { $0 == .claimed } }
    /// Every machine is yours, and nobody waits for a decision.
    var done: Bool { claimed && questions.isEmpty }

    /// Claims each machine not yours yet. Closing the Face ID prompt stops, leaving the rest
    /// ready; a machine that refuses is marked and the others go on.
    func claim() async {
        working = true
        error = nil
        defer { working = false }
        var failures: [String] = []
        for machine in machines where rows[machine.id] != .claimed {
            rows[machine.id] = .claiming
            do {
                try await claim(machine)
                rows[machine.id] = .claimed
            } catch {
                if isCancelled(error) {
                    rows[machine.id] = .ready
                    break
                }
                rows[machine.id] = .failed
                failures.append("\(machine.name): \(describe(error))")
            }
        }
        if !failures.isEmpty { error = failures.joined(separator: "\n") }
    }

    /// Settles each user waiting for a decision, as answered: "It's me" and "Remove" ask their
    /// machine with the passkey; someone else stays as they are.
    func decide() async {
        working = true
        error = nil
        defer { working = false }
        var failures: [String] = []
        for question in questions {
            do {
                if let decision = (answers[question.id] ?? .me).decision {
                    let assertion = try await authenticator.sign(challenge(question.hostId).0)
                    _ = try await send(question.hostId, .decideLeftover(userId: question.userId, decision: decision,
                                                                        assertion: assertion))
                }
                questions.removeAll { $0.id == question.id }
            } catch {
                if isCancelled(error) { break }
                failures.append("“\(question.user)” on \(question.machine): \(describe(error))")
            }
        }
        if !failures.isEmpty { error = failures.joined(separator: "\n") }
    }

    private func claim(_ machine: ClaimPlan.Machine) async throws {
        let (challenge, creation) = try await challenge(machine.hostId)
        let person: NewPerson
        var registered: PasskeyRegistration?
        if let me {
            let assertion = try await authenticator.sign(challenge)
            guard let key = me.keys.first(where: { $0.credentialId == assertion.id }) else {
                throw HerderError.Local(detail: "that passkey is not the one your machines know")
            }
            person = NewPerson(name: plan.name, passkey: .existing(personId: me.id, key: key, assertion: assertion))
        } else {
            guard let creation, let userId = Data(base64URL: creation.userHandle),
                  let bytes = Data(base64URL: creation.challenge)
            else { throw HerderError.Local(detail: "the machine did not offer to make a passkey") }
            let registration = try await authenticator.register(name: plan.name, userId: userId, challenge: bytes)
            registered = registration
            person = NewPerson(name: plan.name, passkey: .register(registration: registration))
        }
        let also = machines.filter { $0.id != machine.id }.map(\.userId)
        guard case .claimed(let personId, let hosts, let leftovers) =
            try await send(machine.hostId, .claimUsers(person: person, also: also))
        else { throw HerderError.Local(detail: "the machine did not say it is yours") }
        if let index = machines.firstIndex(where: { $0.id == machine.id }) { machines[index].machineRows = hosts }
        for leftover in leftovers {
            let question = ClaimPlan.Question(leftover: leftover, hostId: machine.hostId, machine: machine.name)
            questions.append(question)
            answers[question.id] = .me
        }
        if let registered {
            // The machine lists the new passkey's public key, which the others take it by.
            guard case .people(let people) = try await send(machine.hostId, .listPeople),
                  let keys = people.first(where: { $0.personId == personId })?.passkeys,
                  keys.contains(where: { $0.credentialId == registered.id })
            else { throw HerderError.Local(detail: "the machine did not list your new passkey") }
            me = (personId, keys)
        }
    }

    private func challenge(_ hostId: HostId) async throws -> (PasskeyChallenge, PasskeyCreation?) {
        guard case .passkeyChallenge(let challenge, let registration) = try await send(hostId, .passkeyChallenge) else {
            throw HerderError.Local(detail: "the machine did not send a challenge")
        }
        return (challenge, registration)
    }
}
