import Foundation
import Herder
import Synchronization

/// One machine's answer to a usage summary, with the accounts it lists.
struct MachineUsage: Equatable {
    let hostId: HostId
    let name: String
    let accounts: [Account]
    let totals: [UsageTotal]
    let failovers: [FailoverTotal]
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

/// The machines' answers to a usage summary added up: overall, per login and per model.
struct UsageReport: Equatable {
    /// One login's period: every account signed in to it, on any machine, added up.
    struct Login: Equatable {
        var amount = UsageAmount()
        /// Turns that failed on the login's accounts at their limit.
        var limitHits: UInt64 = 0
        /// Sessions moved off the login's accounts at their limit, and onto them off another.
        var failoversOut: UInt64 = 0
        var failoversIn: UInt64 = 0
    }

    struct ModelRow: Equatable, Identifiable {
        var id: String { "\(provider)/\(model)" }
        let provider: Provider
        /// The model in the provider's own naming; empty when the session never named one.
        let model: String
        var amount = UsageAmount()
    }

    private(set) var total = UsageAmount()
    /// Each login's period, by `ProviderAccounts.key`. An account its machine no longer lists
    /// counts in `total` only.
    private(set) var logins: [String: Login] = [:]
    /// Every model with usage in the period, most expensive first.
    private(set) var models: [ModelRow] = []

    init(_ machines: [MachineUsage]) {
        var models: [String: ModelRow] = [:]
        for machine in machines {
            let keys = Dictionary(machine.accounts.map { ($0.accountId, ProviderAccounts.key($0)) }) { first, _ in first }
            for total in machine.totals {
                self.total.add(total)
                let model = ModelRow(provider: total.provider, model: total.model)
                models[model.id, default: model].amount.add(total)
                if let key = keys[total.accountId] { logins[key, default: Login()].amount.add(total) }
            }
            for failover in machine.failovers {
                guard let key = keys[failover.accountId] else { continue }
                logins[key, default: Login()].limitHits += failover.limitHits
                logins[key, default: Login()].failoversOut += failover.failoversOut
                logins[key, default: Login()].failoversIn += failover.failoversIn
            }
        }
        self.models = models.values.sorted { a, b in
            if a.amount.costUsd != b.amount.costUsd { return a.amount.costUsd > b.amount.costUsd }
            if a.amount.tokens != b.amount.tokens { return a.amount.tokens > b.amount.tokens }
            return a.model.localizedStandardCompare(b.model) == .orderedAscending
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

/// The machines' latest answers to a usage summary, per period: shown at once when the
/// Providers screen opens, and asked again only once stale. Plan windows need none of it; they
/// come live with the accounts.
struct UsageCache {
    struct Answer: Equatable {
        let machines: [MachineUsage]
        let failures: [String]
        /// The machines asked; another set connected makes the answer stale.
        let asked: [HostId]
        let at: Date
    }

    /// How long an answer stays fresh.
    static let freshFor: TimeInterval = 60

    private var answers: [UsagePeriod: Answer] = [:]
    /// The periods being asked now, so a second look does not ask again meanwhile.
    private var asking: Set<UsagePeriod> = []

    subscript(period: UsagePeriod) -> Answer? { answers[period] }

    /// Whether to ask `machines` for `period` now: never while asking already, else when no
    /// answer is cached, it asked other machines, or it is older than `freshFor`.
    func isStale(_ period: UsagePeriod, machines: [HostId], now: Date) -> Bool {
        guard !asking.contains(period) else { return false }
        guard let answer = answers[period] else { return true }
        return answer.asked != machines || now.timeIntervalSince(answer.at) >= Self.freshFor
    }

    mutating func asks(_ period: UsagePeriod) {
        asking.insert(period)
    }

    mutating func answered(_ period: UsagePeriod, with answer: Answer) {
        answers[period] = answer
        asking.remove(period)
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
    /// How long a machine gets to add its usage up. A daemon older than the usage summary
    /// never answers it, so without a limit the screen would wait forever.
    static let usageTimeout: Duration = .seconds(10)

    /// The machines asked for their usage: the connected ones; a vault runs no sessions.
    private var usageMachines: [Machine] {
        machines.filter { $0.connection == .connected && $0.hosts.isEmpty }
    }

    /// Asks the machines for their usage over `period` into `usageCache`, unless it holds a
    /// fresh answer; always when `force`.
    func refreshUsage(over period: UsagePeriod, force: Bool = false) async {
        let asked = usageMachines.map(\.hostId)
        guard force || usageCache.isStale(period, machines: asked, now: .now) else { return }
        usageCache.asks(period)
        let (machines, failures) = await usage(over: period)
        usageCache.answered(period, with: .init(machines: machines, failures: failures, asked: asked, at: .now))
    }

    /// Asks every connected machine for its usage over `period`, all at once. A machine that
    /// does not answer in time is left out, with why.
    func usage(over period: UsagePeriod) async -> (machines: [MachineUsage], failures: [String]) {
        let asked = usageMachines
        let client = client
        typealias Summary = (totals: [UsageTotal], failovers: [FailoverTotal])
        let answers = await withTaskGroup(of: (Int, Result<Summary, any Error>).self) { group in
            for (index, machine) in asked.enumerated() {
                let hostId = machine.hostId
                group.addTask {
                    do {
                        let summary = try await answered(within: Self.usageTimeout,
                                                       or: "did not answer; it may run an older herder") {
                            guard case .usageSummary(_, _, let totals, let failovers) = try await client.send(
                                hostId: hostId, command: .getUsageSummary(period: period))
                            else { throw HerderError.Local(detail: "the machine did not add its usage up") }
                            return Summary(totals, failovers)
                        }
                        return (index, .success(summary))
                    } catch {
                        return (index, .failure(error))
                    }
                }
            }
            var answers: [(Int, Result<Summary, any Error>)] = []
            for await answer in group { answers.append(answer) }
            return answers.sorted { $0.0 < $1.0 }.map(\.1)
        }
        var machines: [MachineUsage] = []
        var failures: [String] = []
        for (machine, answer) in zip(asked, answers) {
            switch answer {
            case .success(let summary):
                machines.append(MachineUsage(hostId: machine.hostId, name: machine.name, accounts: machine.accounts,
                                             totals: summary.totals, failovers: summary.failovers))
            case .failure(let error):
                failures.append("\(machine.name): \(describe(error))")
            }
        }
        return (machines, failures)
    }
}

/// `operation`'s answer, or a `.Local` error saying `late` once `limit` passes without one.
/// A call into herder cannot be cancelled, so it runs on and its late answer is dropped.
func answered<T: Sendable>(
    within limit: Duration, or late: String, _ operation: @escaping @Sendable () async throws -> T
) async throws -> T {
    try await withCheckedThrowingContinuation { continuation in
        let once = Once(continuation)
        let timer = Task {
            try await Task.sleep(for: limit)
            once.resume(with: .failure(HerderError.Local(detail: late)))
        }
        Task {
            do { once.resume(with: .success(try await operation())) } catch { once.resume(with: .failure(error)) }
            timer.cancel()
        }
    }
}

/// A continuation resumed by whichever caller comes first.
private final class Once<T: Sendable>: Sendable {
    private let continuation: Mutex<CheckedContinuation<T, any Error>?>

    init(_ continuation: CheckedContinuation<T, any Error>) {
        self.continuation = Mutex(continuation)
    }

    func resume(with result: Result<T, any Error>) {
        continuation.withLock { $0.take() }?.resume(with: result)
    }
}
