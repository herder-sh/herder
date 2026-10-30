import Foundation
import Herder
import Observation
import SwiftUI

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
        /// The herder it runs; nil when too old to say.
        var version: String? = ClaimPlan.needs

        /// It runs a herder that takes the claim.
        var ready: Bool { version.map { !ProviderVersions.version(ClaimPlan.needs, isNewerThan: $0) } ?? false }
    }

    /// The first herder that takes a claim.
    nonisolated static let needs = "0.17.0"

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
    /// What the passkey is saved under: your name; empty when nothing knows it, so the sheet asks.
    var name = "Owner"
    /// Who your passkey may be, when a machine knows people with one: it is reused, not made anew.
    /// The person a machine says this device is, else every owner with a passkey, as iCloud
    /// Keychain gives one device's passkey to your others.
    var known: [PersonInfo] = []
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
    /// "Create your herder passkey, then claim 5 machines", counting those ready.
    var headline: String {
        let ready = machines.filter(\.ready).count
        guard ready > 0 else { return "Update herder on your machines, then claim them" }
        let count = ready == 1 ? "1 machine" : "\(ready) machines"
        return known.isEmpty ? "Create your herder passkey, then claim \(count)" : "Claim \(count) with your herder passkey"
    }

    /// "Stored in iCloud Keychain; Face ID once for all five."
    var detail: String {
        let ready = machines.filter(\.ready).count
        guard ready > 0 else { return "Claiming needs herder \(Self.short(Self.needs)) or newer." }
        let all = ready == 1 ? "it" : ready == 2 ? "both" : "all \(Self.word(ready))"
        return "Stored in \(keychain); \(biometry) once for \(all)."
    }

    /// "0.17" for "0.17.0".
    nonisolated static func short(_ version: String) -> String {
        version.hasSuffix(".0") ? String(version.dropLast(2)) : version
    }

    nonisolated static func word(_ number: Int) -> String {
        let words = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"]
        return number < words.count ? words[number] : "\(number)"
    }

    /// The machines you own that still know you by a per-daemon user: connected, owner, and no
    /// person in their hello. A vault is named apart from the machine it runs on.
    nonisolated static func unclaimed(_ machines: [Herder.Machine]) -> [Machine] {
        machines.compactMap { machine in
            guard machine.role == .owner, machine.personId == nil, machine.connection == .connected,
                  let userId = machine.userId
            else { return nil }
            let vault = !machine.hosts.isEmpty || machine.vault != nil
            let name = vault && !machine.name.lowercased().contains("vault")
                ? UnpairedVault.name(machine: machine.name) : machine.name
            return Machine(hostId: machine.hostId, name: name, userId: userId, version: machine.herderVersion)
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
    /// Some machine you own is not claimed yet, so People and Fleet offer Claim your machines.
    var hasUnclaimed: Bool { !ClaimPlan.unclaimed(machines).isEmpty }

    /// Opens Claim your machines in `sheet`, when a machine is left to claim.
    func offerClaim(in sheet: Binding<AppSheet?>) {
        Task { if let plan = await claimPlan() { sheet.wrappedValue = .claim(plan) } }
    }

    /// Claim your machines, for the machines you own that are not claimed yet; nil when none is.
    func claimPlan() async -> ClaimPlan? {
        let machines = ClaimPlan.unclaimed(self.machines)
        guard !machines.isEmpty else { return nil }
        let people = FleetPeople(fleet: self)
        await people.reload(self.machines)
        let known = Self.yours(FleetPeople.me(self.machines), in: people.people)
        return ClaimPlan(machines: machines, name: known.count == 1 ? known[0].name : Self.deviceUserName,
                         known: known, biometry: Biometry.name)
    }

    /// Who your passkey may be: the person the machines say you are, else every owner with a
    /// passkey, once each.
    nonisolated static func yours(_ me: PersonId?, in people: [HostId: [PersonInfo]]) -> [PersonInfo] {
        if let known = keyed(me, in: people) { return [known] }
        var seen: Set<PersonId> = []
        return people.keys.sorted().flatMap { people[$0] ?? [] }.filter {
            $0.role == .owner && !$0.passkeys.isEmpty && seen.insert($0.personId).inserted
        }
    }
}

/// Claiming: the passkey is made on the first machine (or the one your machines know is used),
/// then each machine takes it with your users on the others in `also`. The users a machine
/// leaves over then wait for your decision.
@MainActor @Observable
final class Claim {
    enum Row: Equatable {
        /// Its herder is too old to take the claim; `updating` once asked to update.
        case outdated(updating: Bool)
        case ready, claiming, claimed
        /// Why it refused.
        case failed(String)

        var failed: Bool { if case .failed = self { true } else { false } }
    }

    typealias Send = @MainActor (HostId, CommandBody) async throws -> CommandResult
    /// Updates a machine's herder, when idle, through Activity.
    typealias Update = @MainActor (HostId) -> Void
    /// Reconnects to a machine once claimed, so its hello names the person.
    typealias Reconnect = @MainActor (HostId) async -> Void

    let plan: ClaimPlan
    /// The machines, with the machine rows each moved once claimed.
    private(set) var machines: [ClaimPlan.Machine]
    /// The users waiting for a decision.
    private(set) var questions: [ClaimPlan.Question]
    var answers: [String: ClaimPlan.Answer]
    private(set) var rows: [HostId: Row]
    private(set) var working = false
    /// Why a user's decision failed, by question.
    private(set) var refusals: [String: String] = [:]
    /// What a new passkey is saved under, which the sheet asks for when nothing knows it.
    var name: String
    /// You, once a machine knows your passkey: the person with its public keys.
    private var me: PersonInfo?
    private let authenticator: Authenticator
    private let send: Send
    private let updateHerder: Update
    private let reconnect: Reconnect

    init(_ plan: ClaimPlan, authenticator: Authenticator, send: @escaping Send, update: @escaping Update = { _ in },
         reconnect: @escaping Reconnect = { _ in }) {
        self.plan = plan
        self.authenticator = authenticator
        self.send = send
        self.updateHerder = update
        self.reconnect = reconnect
        name = plan.name
        machines = plan.machines
        questions = plan.questions
        answers = Dictionary(uniqueKeysWithValues: plan.questions.map { ($0.id, .me) })
        rows = Dictionary(uniqueKeysWithValues: plan.machines.map { ($0.id, $0.ready ? .ready : .outdated(updating: false)) })
        me = plan.known.count == 1 ? plan.known[0] : nil
    }

    /// Every machine is yours.
    var claimed: Bool { rows.values.allSatisfy { $0 == .claimed } }
    /// Every machine is yours, and nobody waits for a decision.
    var done: Bool { claimed && questions.isEmpty }
    /// Some machine not yours yet takes the claim.
    var claimable: Bool { rows.values.contains { $0 == .ready || $0 == .claiming || $0.failed } }
    /// The machines whose herder is too old, not asked to update yet.
    var outdated: [ClaimPlan.Machine] { machines.filter { rows[$0.id] == .outdated(updating: false) } }

    /// Claims each machine not yours yet whose herder takes it: the passkey is made on the
    /// first when no machine knows one, then one assertion over every other machine's challenge
    /// claims them all, so Face ID is asked once for the rest. A Face ID prompt closed or timed
    /// out stops, failing the machines it was for; a machine that refuses is marked and the
    /// others go on.
    func claim() async {
        working = true
        defer { working = false }
        var left = machines.filter { $0.ready && rows[$0.id] != .claimed }[...]
        for machine in left { rows[machine.id] = .claiming }
        do {
            while me == nil, plan.known.isEmpty, let machine = left.popFirst() {
                try await attempt(machine) { try await self.register(machine) }
            }
            try await claimSigned(Array(left))
        } catch {
            // The system reports a prompt that timed out as closed, so neither is silent.
            for machine in machines where rows[machine.id] == .claiming {
                rows[machine.id] = .failed("the passkey prompt was closed or timed out before it signed; claim again")
            }
        }
    }

    /// Updates `hostId`'s herder when idle; its row is ready once it is back on a newer one.
    func update(_ hostId: HostId) {
        guard rows[hostId] == .outdated(updating: false) else { return }
        rows[hostId] = .outdated(updating: true)
        updateHerder(hostId)
    }

    /// Updates every machine whose herder is too old.
    func updateAll() {
        for machine in outdated { update(machine.id) }
    }

    /// Takes the versions the machines run now: a machine back on a herder that takes the
    /// claim becomes ready.
    func refresh(_ fleet: [Herder.Machine]) {
        for index in machines.indices {
            guard let listed = fleet.first(where: { $0.hostId == machines[index].hostId }),
                  listed.herderVersion != machines[index].version
            else { continue }
            machines[index].version = listed.herderVersion
            if case .outdated = rows[machines[index].id], machines[index].ready { rows[machines[index].id] = .ready }
        }
    }

    /// Settles each user waiting for a decision, as answered: "It's me" and "Remove" ask their
    /// machine with the passkey; someone else stays as they are.
    func decide() async {
        working = true
        refusals = [:]
        defer { working = false }
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
                refusals[question.id] = describe(error)
            }
        }
    }

    /// Runs `body` for `machine`, marking it claimed; a refusal marks it failed with why, and a
    /// closed or timed-out Face ID prompt is thrown.
    private func attempt(_ machine: ClaimPlan.Machine, _ body: () async throws -> Void) async throws {
        do {
            try await body()
            rows[machine.id] = .claimed
        } catch {
            if isCancelled(error) { throw error }
            rows[machine.id] = .failed(describe(error))
        }
    }

    /// Makes your passkey from `machine`'s challenge, and claims it with it.
    private func register(_ machine: ClaimPlan.Machine) async throws {
        guard case (_, let creation?) = try await challenge(machine.hostId),
              let userId = Data(base64URL: creation.userHandle), let bytes = Data(base64URL: creation.challenge)
        else { throw HerderError.Local(detail: "the machine did not offer to make a passkey") }
        let name = name.trimmingCharacters(in: .whitespaces)
        let registration = try await authenticator.register(name: name, userId: userId, challenge: bytes)
        let personId = try await take(machine, NewPerson(name: name, passkey: .register(registration: registration)))
        // The machine lists the new passkey's public key, which the others take it by.
        guard case .people(let people) = try await send(machine.hostId, .listPeople),
              let person = people.first(where: { $0.personId == personId }),
              person.passkeys.contains(where: { $0.credentialId == registration.id })
        else { throw HerderError.Local(detail: "the machine did not list your new passkey") }
        me = person
    }

    /// Claims `machines` with one assertion over all their challenges, one after the other,
    /// each machine finding its own among them.
    private func claimSigned(_ machines: [ClaimPlan.Machine]) async throws {
        var challenges: [(machine: ClaimPlan.Machine, bytes: Data)] = []
        for machine in machines {
            do {
                guard let bytes = Data(base64URL: try await challenge(machine.hostId).0.challenge) else {
                    throw HerderError.Local(detail: "the machine sent a challenge that is not base64url")
                }
                challenges.append((machine, bytes))
            } catch {
                rows[machine.id] = .failed(describe(error))
            }
        }
        guard !challenges.isEmpty else { return }
        let assertion = try await authenticator.assert(challenge: challenges.map(\.bytes).reduce(Data(), +))
        let candidates = me.map { [$0] } ?? plan.known
        guard let person = candidates.first(where: { $0.passkeys.contains { $0.credentialId == assertion.id } }),
              let key = person.passkeys.first(where: { $0.credentialId == assertion.id })
        else {
            for (machine, _) in challenges { rows[machine.id] = .failed("that passkey is not the one your machines know") }
            return
        }
        me = person
        let passkey = PersonPasskey.existing(personId: person.personId, key: key, assertion: assertion)
        for (machine, _) in challenges {
            try await attempt(machine) { _ = try await self.take(machine, NewPerson(name: person.name, passkey: passkey)) }
        }
    }

    /// Has `machine` take `person` with your users on the others, then reconnects to it so its
    /// hello names you; returns the person.
    private func take(_ machine: ClaimPlan.Machine, _ person: NewPerson) async throws -> PersonId {
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
        await reconnect(machine.hostId)
        return personId
    }

    private func challenge(_ hostId: HostId) async throws -> (PasskeyChallenge, PasskeyCreation?) {
        guard case .passkeyChallenge(let challenge, let registration) = try await send(hostId, .passkeyChallenge) else {
            throw HerderError.Local(detail: "the machine did not send a challenge")
        }
        return (challenge, registration)
    }
}
