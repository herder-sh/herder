import Foundation
import Herder
import Observation

/// The one-time move to one identity: on each machine you own, your old user (or users) become
/// your passkey. Machines that show up as users move to the vault's Machines list, and users
/// nobody can place wait for your decision. Offered once on launch while a machine you own is
/// not claimed yet.
struct ClaimPlan: Equatable {
    /// A machine that becomes yours.
    struct Machine: Equatable, Identifiable {
        var id: String { name }
        let name: String
        /// Your users there now, with their roles: "tomas" owner, "owner" owner.
        let users: [String]
        /// The users there that are machines, which move to the vault's Machines list.
        var machineRows: [String] = []
    }

    /// A user on a machine that might be you, or someone else.
    struct Question: Equatable, Identifiable {
        var id: String { "\(user)@\(machine)" }
        let user: String
        let machine: String
        /// Why herder cannot tell: "A user from Oct 9 whose CLI device is online."
        let reason: String
    }

    enum Answer: String, CaseIterable, Equatable {
        case me, teammate, remove

        var title: String {
            switch self {
            case .me: "It's me"
            case .teammate: "Keep as a teammate"
            case .remove: "Remove"
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

    /// "Stored in iCloud Keychain; Face ID once for all five."
    var detail: String {
        let all = machines.count == 1 ? "it" : machines.count == 2 ? "both" : "all \(Self.word(machines.count))"
        return "Stored in \(keychain); \(biometry) once for \(all)."
    }

    nonisolated static func word(_ number: Int) -> String {
        let words = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"]
        return number < words.count ? words[number] : "\(number)"
    }

    /// The mockup's: five machines, one "owner" nobody can place.
    static let demo = ClaimPlan(
        machines: [
            Machine(name: "This Mac", users: ["tomas"]),
            Machine(name: "trash-can-01", users: ["tomas"]),
            Machine(name: "draugelis", users: ["tomas"]),
            Machine(name: "herder-vault-0", users: ["tomas", "owner"],
                    machineRows: ["trash-can-01", "Coingate-CJH9FXV367", "draugelis"]),
            Machine(name: "draugelis vault", users: ["tomas"], machineRows: ["draugelis"]),
        ],
        questions: [Question(user: "owner", machine: "herder-vault-0",
                             reason: "A user from Oct 9 whose CLI device is online. Is this you?")])
}

/// Claiming: the passkey is created, then each machine is claimed in turn with it.
@MainActor @Observable
final class Claim {
    enum Row: Equatable {
        case ready, claiming, claimed
        case failed(String)
    }

    typealias Send = @MainActor (ClaimPlan, [String: ClaimPlan.Answer], PasskeyRegistration) async throws -> Void

    let plan: ClaimPlan
    var answers: [String: ClaimPlan.Answer]
    private(set) var rows: [String: Row]
    private(set) var working = false
    private(set) var error: String?
    private let authenticator: Authenticator
    private let send: Send

    init(_ plan: ClaimPlan, authenticator: Authenticator, send: @escaping Send) {
        self.plan = plan
        self.authenticator = authenticator
        self.send = send
        answers = Dictionary(uniqueKeysWithValues: plan.questions.map { ($0.id, .me) })
        rows = Dictionary(uniqueKeysWithValues: plan.machines.map { ($0.id, .ready) })
    }

    var done: Bool { rows.values.allSatisfy { $0 == .claimed } }

    func claim(name: String, userId: Data, challenge: Data) async {
        working = true
        error = nil
        defer { working = false }
        do {
            let passkey = try await authenticator.register(name: name, userId: userId, challenge: challenge)
            for machine in plan.machines { rows[machine.id] = .claiming }
            try await send(plan, answers, passkey)
            for machine in plan.machines { rows[machine.id] = .claimed }
        } catch {
            for machine in plan.machines { rows[machine.id] = .ready }
            if !isCancelled(error) { self.error = describe(error) }
        }
    }
}
