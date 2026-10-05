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

/// What the Usage screen shows: the machines' answers added up, overall, per account and per
/// model, for all machines or one.
struct UsageReport: Equatable {
    struct AccountRow: Equatable, Identifiable {
        var id: String { "\(hostId)/\(accountId)" }
        let hostId: HostId
        let accountId: AccountId
        let label: String
        let provider: Provider
        let machine: String
        var amount = UsageAmount()
        /// The plan's session window (five hours), when the provider reports one.
        var session: WindowLeft?
        /// The plan's weekly window, when the provider reports one.
        var weekly: WindowLeft?
    }

    struct ModelRow: Equatable, Identifiable {
        var id: String { "\(provider)/\(model)" }
        let provider: Provider
        /// The model in the provider's own naming; empty when the session never named one.
        let model: String
        var amount = UsageAmount()
    }

    private(set) var total = UsageAmount()
    /// Every account with usage in the period or a plan window, most expensive first.
    private(set) var accounts: [AccountRow] = []
    /// Every model with usage in the period, most expensive first.
    private(set) var models: [ModelRow] = []

    /// Adds `machines` up; only `hostId`'s when given.
    init(_ machines: [MachineUsage], only hostId: HostId? = nil, now: Date = .now) {
        var models: [String: ModelRow] = [:]
        for machine in machines where hostId == nil || machine.hostId == hostId {
            var rows = machine.accounts.map { account in
                AccountRow(
                    hostId: machine.hostId, accountId: account.accountId, label: account.label,
                    provider: account.provider, machine: machine.name,
                    session: Self.window("Session", of: account, now: now),
                    weekly: Self.window("Weekly", of: account, now: now))
            }
            for total in machine.totals {
                self.total.add(total)
                let model = ModelRow(provider: total.provider, model: total.model)
                models[model.id, default: model].amount.add(total)
                if let index = rows.firstIndex(where: { $0.accountId == total.accountId }) {
                    rows[index].amount.add(total)
                } else {
                    // An account the machine no longer lists keeps its usage.
                    var row = AccountRow(hostId: machine.hostId, accountId: total.accountId, label: total.accountId,
                                         provider: total.provider, machine: machine.name)
                    row.amount.add(total)
                    rows.append(row)
                }
            }
            accounts += rows.filter { $0.amount.turns > 0 || $0.session != nil || $0.weekly != nil }
        }
        accounts.sort { Self.costlier($0.amount, $1.amount, $0.label, $1.label) }
        self.models = models.values.sorted { Self.costlier($0.amount, $1.amount, $0.model, $1.model) }
    }

    private static func costlier(_ a: UsageAmount, _ b: UsageAmount, _ aName: String, _ bName: String) -> Bool {
        if a.costUsd != b.costUsd { return a.costUsd > b.costUsd }
        if a.tokens != b.tokens { return a.tokens > b.tokens }
        return aName.localizedStandardCompare(bName) == .orderedAscending
    }

    /// The account's window that the Machines screen labels `label`.
    private static func window(_ label: String, of account: Account, now: Date) -> WindowLeft? {
        account.usage.first { Lists.usageLabel($0.window) == label }.map { window in
            WindowLeft(percentUsed: window.usedPercent,
                       resets: Timestamp.until(window.resetsAt.flatMap(Timestamp.date), now: now))
        }
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
