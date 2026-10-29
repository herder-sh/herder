import Foundation
import Herder
import Observation

/// The one-time move to one identity: on each machine you own, your old user becomes your
/// passkey. Machines that show up as users move to the vault's Machines list, and users nobody
/// can place wait for your decision. Offered once a launch while a machine you own is not
/// claimed yet.
struct ClaimPlan: Hashable {
    /// A machine that becomes yours.
    struct Machine: Hashable, Identifiable {
        let id: HostId
        let name: String
        /// Your user there now: what becomes your passkey.
        let me: UserId
        /// Your users' names there: "tomas".
        let users: [String]
        /// The users there that are machines, which move to the vault's Machines list.
        var machineRows: [String] = []
    }

    /// A user on a machine that might be you, or someone else.
    struct Question: Hashable, Identifiable {
        var id: String { "\(userId)@\(hostId)" }
        let hostId: HostId
        let machine: String
        let userId: UserId
        let user: String
        /// Why herder cannot tell: "A user from Oct 9 whose CLI device is online. Is this you?"
        let reason: String
        /// What the sheet suggests: you, for another owner; someone else, for a member.
        let suggested: Answer
    }

    enum Answer: String, CaseIterable, Hashable {
        case me, someoneElse, remove

        var title: String {
            switch self {
            case .me: "It's me"
            case .someoneElse: "Someone else"
            case .remove: "Remove"
            }
        }

        /// What the machine is told; someone else stays a user of their own until invited.
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
    /// Where the passkey will be kept.
    var keychain = "iCloud Keychain"
    /// What unlocks it: "Face ID".
    var biometry = "Face ID"
}

extension ClaimPlan.Machine {
    /// "tomas (owner) → you · 3 machine rows move to Machines".
    var change: String {
        let from = "\(users.joined(separator: ", ")) (owner) → you"
        switch machineRows.count {
        case 0: return from
        case 1: return "\(from) · \(machineRows[0]) moves to Machines"
        case let count: return "\(from) · \(count) machine rows move to Machines"
        }
    }
}

extension ClaimPlan {
    /// "Create your herder passkey, then claim 5 machines".
    var headline: String {
        "Create your herder passkey, then claim \(machines.count == 1 ? "1 machine" : "\(machines.count) machines")"
    }

    /// "Stored in iCloud Keychain; Face ID once for each machine." Each machine signs its own
    /// challenge, so there is no one prompt for all of them.
    var detail: String {
        "Stored in \(keychain); \(biometry) once for \(machines.count == 1 ? "it" : "each machine")."
    }

    /// What claiming moves on one machine you own, from its users, or nil when none of them is
    /// this device's: you are the owner with this app's device online; users whose every device
    /// is a host's are machines; everyone else is a question.
    nonisolated static func machine(_ hostId: HostId, name: String, users: [UserInfo], thisClient: String)
        -> (Machine, [Question])? {
        guard let me = users.first(where: { user in
            user.role == .owner && user.devices.contains { $0.online && $0.client.hasPrefix(thisClient + "/") }
        }) else { return nil }
        let others = users.filter { $0.userId != me.userId }
        let isMachine = { (user: UserInfo) in !user.devices.isEmpty && user.devices.allSatisfy(\.hostOnly) }
        let machine = Machine(id: hostId, name: name, me: me.userId, users: [me.name],
                              machineRows: others.filter(isMachine).map(\.name))
        let questions = others.filter { !isMachine($0) }.map { user in
            Question(hostId: hostId, machine: name, userId: user.userId, user: user.name, reason: reason(user),
                     suggested: user.role == .owner ? .me : .someoneElse)
        }
        return (machine, questions)
    }

    /// "A user from Oct 9 whose CLI device is online. Is this you?"
    nonisolated static func reason(_ user: UserInfo) -> String {
        guard let device = user.devices.first else { return "A user with no devices. Is this you?" }
        let since = Timestamp.date(device.pairedAt).map { " from \($0.formatted(.dateTime.month(.abbreviated).day()))" } ?? ""
        let kind = switch device.client.split(separator: "/").first ?? "" {
        case "herder-ios": "iPhone"
        case "herder-macos": "Mac"
        case "herder-tui": "terminal"
        default: "CLI"
        }
        return "A user\(since) whose \(kind) device is \(device.online ? "online" : "offline"). Is this you?"
    }

    /// The mockup's: five machines, one "owner" nobody can place.
    static let demo = ClaimPlan(
        machines: [
            Machine(id: "mac", name: "This Mac", me: "u-mac", users: ["tomas"]),
            Machine(id: "trash", name: "trash-can-01", me: "u-trash", users: ["tomas"]),
            Machine(id: "draugelis", name: "draugelis", me: "u-draugelis", users: ["tomas"]),
            Machine(id: "vault", name: "herder-vault-0", me: "u-vault", users: ["tomas", "owner"],
                    machineRows: ["trash-can-01", "Coingate-CJH9FXV367", "draugelis"]),
            Machine(id: "dvault", name: "draugelis vault", me: "u-dvault", users: ["tomas"], machineRows: ["draugelis"]),
        ],
        questions: [Question(hostId: "vault", machine: "herder-vault-0", userId: "u-owner", user: "owner",
                             reason: "A user from Oct 9 whose CLI device is online. Is this you?", suggested: .me)])
}

/// Claiming: the passkey is made on the first machine, then each other machine is claimed with
/// it, and the leftovers you answered are settled. Every machine signs its own challenge.
@MainActor @Observable
final class Claim {
    enum Row: Equatable {
        case ready, claiming, claimed
        case failed(String)
    }

    /// A command to a machine, and its result.
    typealias Send = @MainActor (HostId, CommandBody) async throws -> CommandResult

    /// You, once a machine knows your passkey: who you are and your passkeys' public keys.
    struct Person: Hashable {
        let id: PersonId
        let name: String
        let keys: [PasskeyKey]
    }

    let plan: ClaimPlan
    var answers: [String: ClaimPlan.Answer]
    private(set) var rows: [HostId: Row]
    private(set) var working = false
    /// Your name, as the passkey is saved under it.
    private let name: String
    private var person: Person?
    private let authenticator: Authenticator
    private let send: Send

    /// `person` is you when a machine already knows your passkey, as after approving a new one;
    /// a second passkey would make a second person.
    init(_ plan: ClaimPlan, name: String, person: Person? = nil, authenticator: Authenticator, send: @escaping Send) {
        self.plan = plan
        self.name = name
        self.person = person
        self.authenticator = authenticator
        self.send = send
        answers = Dictionary(uniqueKeysWithValues: plan.questions.map { ($0.id, $0.suggested) })
        rows = Dictionary(uniqueKeysWithValues: plan.machines.map { ($0.id, .ready) })
    }

    var done: Bool { rows.values.allSatisfy { $0 == .claimed } }

    /// Claims every machine not claimed yet. One that fails keeps its error and the rest go on;
    /// closing the Face ID prompt stops, leaving the rest ready.
    func claim() async {
        working = true
        defer { working = false }
        for machine in plan.machines where rows[machine.id] != .claimed {
            rows[machine.id] = .claiming
            do {
                try await claim(machine)
                rows[machine.id] = .claimed
            } catch {
                if isCancelled(error) {
                    rows[machine.id] = .ready
                    return
                }
                rows[machine.id] = .failed(describe(error))
            }
        }
    }

    private func claim(_ machine: ClaimPlan.Machine) async throws {
        let (challenge, creation) = try await self.challenge(machine.id)
        let you: NewPerson
        var registered: PasskeyRegistration?
        if let person {
            let assertion = try await authenticator.sign(challenge)
            guard let key = person.keys.first(where: { $0.credentialId == assertion.id }) else {
                throw HerderError.Local(detail: "that passkey is not the one your machines know")
            }
            you = NewPerson(name: name, passkey: .existing(personId: person.id, key: key, assertion: assertion))
        } else {
            guard let creation, let userId = Data(base64URL: creation.userHandle),
                  let bytes = Data(base64URL: creation.challenge)
            else { throw HerderError.Local(detail: "\(machine.name) did not offer to make a passkey") }
            let registration = try await authenticator.register(name: name, userId: userId, challenge: bytes)
            registered = registration
            you = NewPerson(name: name, passkey: .register(registration: registration))
        }
        let also = plan.machines.filter { $0.id != machine.id }.map(\.me)
        guard case .claimed(let personId, _, let leftovers) = try await send(machine.id, .claimUsers(person: you, also: also))
        else { throw HerderError.Local(detail: "\(machine.name) did not claim") }
        if let registered { person = try await learn(personId, registered: registered, on: machine.id) }
        for question in plan.questions where question.hostId == machine.id {
            guard let decision = answers[question.id]?.decision,
                  leftovers.contains(where: { $0.userId == question.userId }) else { continue }
            let assertion = try await authenticator.sign(try await self.challenge(machine.id).0)
            _ = try await send(machine.id, .decideLeftover(userId: question.userId, decision: decision, assertion: assertion))
        }
    }

    private func challenge(_ hostId: HostId) async throws -> (PasskeyChallenge, PasskeyCreation?) {
        guard case .passkeyChallenge(let challenge, let creation) = try await send(hostId, .passkeyChallenge)
        else { throw HerderError.Local(detail: "the machine did not send a challenge") }
        return (challenge, creation)
    }

    /// You with the passkey just made, as the machine that took it lists you.
    private func learn(_ personId: PersonId, registered: PasskeyRegistration, on hostId: HostId) async throws -> Person {
        guard case .people(let people) = try await send(hostId, .listPeople),
              let keys = people.first(where: { $0.personId == personId })?.passkeys,
              keys.contains(where: { $0.credentialId == registered.id })
        else { throw HerderError.Local(detail: "the machine did not list your new passkey") }
        return Person(id: personId, name: name, keys: keys)
    }
}

extension Fleet {
    /// What Claim your machines would move: the connected machines you own that still know you
    /// by a user of their own, whom only they give what to make a passkey from.
    func claimPlan() async -> ClaimPlan {
        var plan = ClaimPlan(machines: [], biometry: Biometry.name)
        let local = MachineRow.thisMac(machines.filter { $0.hosts.isEmpty && $0.vault == nil }, names: MachineRow.localNames)
        for machine in machines where machine.connection == .connected && machine.role == .owner {
            guard case .passkeyChallenge(_, _?)? = try? await client.send(hostId: machine.hostId, command: .passkeyChallenge),
                  case .users(let users)? = try? await client.send(hostId: machine.hostId, command: .listUsers),
                  let (row, questions) = ClaimPlan.machine(machine.hostId, name: machine.hostId == local ? "This Mac" : machine.name,
                                                           users: users, thisClient: FleetPeople.thisClient)
            else { continue }
            plan.machines.append(row)
            plan.questions += questions
        }
        return plan
    }
}

extension Fleet {
    /// You, when a connected machine knows your passkey.
    func claimant() async -> Claim.Person? {
        let people = FleetPeople(fleet: self)
        await people.reload(machines)
        return Self.keyed(FleetPeople.me(people.people, machines: machines), in: people.people)
            .map { Claim.Person(id: $0.personId, name: $0.name, keys: $0.passkeys) }
    }
}
