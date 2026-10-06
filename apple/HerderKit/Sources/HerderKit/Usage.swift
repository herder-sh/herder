import Foundation
import Herder

/// One machine's answer to a usage summary, with the accounts it lists.
struct MachineUsage: Equatable {
    let hostId: HostId
    let name: String
    let accounts: [Account]
    let totals: [UsageTotal]
}

/// Tokens and API-equivalent dollars, added up.
struct UsageAmount: Equatable {
    var turns: UInt64 = 0
    var input: UInt64 = 0
    var output: UInt64 = 0
    var cacheRead: UInt64 = 0
    var cacheWrite: UInt64 = 0
    var costUsd: Double = 0
    /// Whether some of `costUsd` is herder's estimate, or some turn's cost was unknown.
    var estimated = false

    var tokens: UInt64 { input + output + cacheRead + cacheWrite }

    /// The share of the input read from the prompt cache instead of sent afresh: what the
    /// cache saved. `nil` with no input.
    var cacheSavings: Double? {
        let all = input + cacheRead + cacheWrite
        return all == 0 ? nil : Double(cacheRead) / Double(all)
    }

    mutating func add(_ total: UsageTotal) {
        turns += total.turns
        input += total.input
        output += total.output
        cacheRead += total.cacheRead
        cacheWrite += total.cacheWrite
        costUsd += total.costUsd
        estimated = estimated || total.costEstimated
    }
}

/// How much of a plan's limit window is left, and when it resets.
struct WindowLeft: Equatable {
    let percentUsed: Double
    let resets: String

    var percentLeft: Double { max(0, 100 - percentUsed) }
}

/// What the Usage screen shows: the machines' answers added up, overall, per login and per
/// model, for all machines or one.
struct UsageReport: Equatable {
    /// One provider login: every account signed in to it, on any machine, added up. Accounts
    /// with the same provider and email share a login and its plan windows; an account whose
    /// email is unknown is a login of its own.
    struct AccountRow: Equatable, Identifiable {
        /// The provider and email, or for an account without an email, its machine and id.
        let id: String
        let provider: Provider
        /// The email the login is signed in as, when its CLI reported one.
        let email: String?
        /// The labels of its accounts, and their machines, each once, in the order first met.
        private(set) var labels: [String] = []
        private(set) var machines: [String] = []
        var amount = UsageAmount()
        /// The plan's session window (five hours), when the provider reports one.
        private(set) var session: WindowLeft?
        /// The plan's weekly window, when the provider reports one.
        private(set) var weekly: WindowLeft?
        /// Every window its accounts reported, as each account's machine last heard it.
        fileprivate var windows: [UsageWindow] = []

        /// The email, or the label of an account without one.
        var title: String { email ?? labels.first ?? "" }

        fileprivate init(id: String, provider: Provider, email: String?) {
            self.id = id
            self.provider = provider
            self.email = email
        }

        fileprivate mutating func add(label: String, machine: String) {
            if !labels.contains(label) { labels.append(label) }
            if !machines.contains(machine) { machines.append(machine) }
        }

        /// The session and weekly windows: of each, the one that resets last, as the others
        /// are older reports of it; the more used one when they reset together.
        fileprivate mutating func settle(now: Date) {
            func latest(_ label: String) -> WindowLeft? {
                let reset = { (window: UsageWindow) in window.resetsAt.flatMap(Timestamp.date) ?? .distantPast }
                return windows.filter { Lists.usageLabel($0.window) == label }
                    .max { (reset($0), $0.usedPercent) < (reset($1), $1.usedPercent) }
                    .map { WindowLeft(percentUsed: $0.usedPercent,
                                      resets: Timestamp.until($0.resetsAt.flatMap(Timestamp.date), now: now)) }
            }
            session = latest("Session")
            weekly = latest("Weekly")
        }
    }

    struct ModelRow: Equatable, Identifiable {
        var id: String { "\(provider)/\(model)" }
        let provider: Provider
        /// The model in the provider's own naming; empty when the session never named one.
        let model: String
        var amount = UsageAmount()
    }

    private(set) var total = UsageAmount()
    /// Every login with usage in the period or a plan window, most expensive first.
    private(set) var accounts: [AccountRow] = []
    /// Every model with usage in the period, most expensive first.
    private(set) var models: [ModelRow] = []

    /// Adds `machines` up; only `hostId`'s when given.
    init(_ machines: [MachineUsage], only hostId: HostId? = nil, now: Date = .now) {
        var models: [String: ModelRow] = [:]
        var logins: [String: AccountRow] = [:]
        func login(_ id: String, provider: Provider, email: String?) -> String {
            if logins[id] == nil { logins[id] = AccountRow(id: id, provider: provider, email: email) }
            return id
        }
        for machine in machines where hostId == nil || machine.hostId == hostId {
            // Each of the machine's accounts, by id, to its login.
            var ids: [AccountId: String] = [:]
            for account in machine.accounts {
                let id = login(account.email.map { "\(account.provider)/\($0)" } ?? "\(machine.hostId)/\(account.accountId)",
                               provider: account.provider, email: account.email)
                ids[account.accountId] = id
                logins[id]?.add(label: account.label, machine: machine.name)
                logins[id]?.windows += account.usage
            }
            for total in machine.totals {
                self.total.add(total)
                let model = ModelRow(provider: total.provider, model: total.model)
                models[model.id, default: model].amount.add(total)
                let id = ids[total.accountId] ?? {
                    // An account the machine no longer lists keeps its usage.
                    let id = login("\(machine.hostId)/\(total.accountId)", provider: total.provider, email: nil)
                    logins[id]?.add(label: total.accountId, machine: machine.name)
                    return id
                }()
                logins[id]?.amount.add(total)
            }
        }
        accounts = logins.values.map { row in
            var row = row
            row.settle(now: now)
            return row
        }
        .filter { $0.amount.turns > 0 || $0.session != nil || $0.weekly != nil }
        .sorted { Self.costlier($0.amount, $1.amount, $0.title, $1.title) }
        self.models = models.values.sorted { Self.costlier($0.amount, $1.amount, $0.model, $1.model) }
    }

    private static func costlier(_ a: UsageAmount, _ b: UsageAmount, _ aName: String, _ bName: String) -> Bool {
        if a.costUsd != b.costUsd { return a.costUsd > b.costUsd }
        if a.tokens != b.tokens { return a.tokens > b.tokens }
        return aName.localizedStandardCompare(bName) == .orderedAscending
    }

    /// A token count, compactly: "950", "12.3K", "4.5M", "1.2B".
    static func tokens(_ count: UInt64) -> String {
        let value = Double(count)
        switch count {
        case ..<1_000: return "\(count)"
        case ..<1_000_000: return String(format: "%.1fK", value / 1_000)
        case ..<1_000_000_000: return String(format: "%.1fM", value / 1_000_000)
        default: return String(format: "%.1fB", value / 1_000_000_000)
        }
    }

    /// Dollars to the cent, "≈" in front when estimated.
    static func dollars(_ amount: UsageAmount) -> String {
        let value = amount.costUsd > 0 && amount.costUsd < 0.01 ? "<$0.01" : String(format: "$%.2f", amount.costUsd)
        return amount.estimated ? "≈ \(value)" : value
    }
}

extension UsagePeriod {
    /// The periods in the picker, shortest first.
    static let all: [UsagePeriod] = [.day, .week, .thirtyDays, .month]

    var label: String {
        switch self {
        case .day: "24h"
        case .week: "7d"
        case .thirtyDays: "30d"
        case .month: "Month"
        }
    }

    var title: String {
        switch self {
        case .day: "Last 24 hours"
        case .week: "Last 7 days"
        case .thirtyDays: "Last 30 days"
        case .month: "Month to date"
        }
    }
}

extension Fleet {
    /// Asks every connected machine for its usage over `period`, one after another. A machine
    /// that does not answer is left out, with why; a vault runs no sessions, so it is not asked.
    func usage(over period: UsagePeriod) async -> (machines: [MachineUsage], failures: [String]) {
        var answers: [MachineUsage] = []
        var failures: [String] = []
        for machine in machines where machine.connection == .connected && machine.hosts.isEmpty {
            do {
                guard case .usageSummary(_, _, let totals) = try await client.send(
                    hostId: machine.hostId, command: .getUsageSummary(period: period))
                else { throw HerderError.Local(detail: "the machine did not add its usage up") }
                answers.append(MachineUsage(hostId: machine.hostId, name: machine.name,
                                            accounts: machine.accounts, totals: totals))
            } catch {
                failures.append("\(machine.name): \(describe(error))")
            }
        }
        return (answers, failures)
    }
}
