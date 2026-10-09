import Foundation
import Herder
@testable import HerderKit
import Testing

private let now = Date(timeIntervalSince1970: 1_767_240_000)

private func total(
    _ account: AccountId, _ model: String, provider: Provider = "claude", turns: UInt64 = 1, input: UInt64 = 100,
    cost: Double = 1, estimated: Bool = false
) -> UsageTotal {
    UsageTotal(accountId: account, provider: provider, model: model, turns: turns, input: input, output: 10,
               cacheRead: 300, cacheWrite: 0, costUsd: cost, costEstimated: estimated)
}

private func account(
    _ id: AccountId, provider: Provider = "claude", email: String? = nil, windows: [UsageWindow] = []
) -> Account {
    Account(accountId: id, provider: provider, label: id.capitalized, configDir: nil, email: email, usage: windows)
}

private func failover(_ account: AccountId, hits: UInt64 = 0, out: UInt64 = 0, in into: UInt64 = 0) -> FailoverTotal {
    FailoverTotal(accountId: account, limitHits: hits, failoversOut: out, failoversIn: into)
}

struct UsageReportTests {
    /// One Claude login on both machines under two accounts; an email-less Codex account on
    /// the laptop.
    private let accounts = (
        desk: [account("main", email: "dev@example.com"), account("idle")],
        laptop: [account("work", email: "dev@example.com"), account("gpt", provider: "codex")]
    )

    private var answers: [MachineUsage] {
        [
            MachineUsage(hostId: "desk", name: "desk", accounts: accounts.desk,
                         totals: [total("main", "opus", turns: 2, input: 200, cost: 2)],
                         failovers: [failover("main", hits: 3, out: 2), failover("idle", in: 2)]),
            MachineUsage(hostId: "laptop", name: "laptop", accounts: accounts.laptop,
                         totals: [
                             total("work", "opus", cost: 0.5),
                             total("gpt", "gpt-5", provider: "codex", cost: 3, estimated: true),
                         ],
                         failovers: [failover("work", hits: 1, in: 1)]),
        ]
    }

    @Test func usageAndFailoversJoinTheLoginsTheProvidersScreenLists() throws {
        let report = UsageReport(answers)
        #expect(report.total == UsageAmount(turns: 4, input: 400, output: 30, cacheRead: 900, cacheWrite: 0,
                                            costUsd: 5.5, estimated: true))
        let logins = ProviderAccounts(machines: [
            machine("desk", name: "desk", sessions: [], accounts: accounts.desk),
            machine("laptop", name: "laptop", sessions: [], accounts: accounts.laptop),
        ]).groups.flatMap(\.logins)
        // Every login the screen lists finds its usage under its own id.
        #expect(Set(report.logins.keys) == Set(logins.map(\.id)))
        let claude = try #require(report.logins["claude/dev@example.com"])
        #expect(claude.amount.turns == 3)
        #expect(claude.amount.costUsd == 2.5)
        #expect((claude.limitHits, claude.failoversOut, claude.failoversIn) == (4, 2, 1))
        let idle = try #require(report.logins["claude/id:idle"])
        #expect(idle.amount.turns == 0)
        #expect(idle.failoversIn == 2)
        #expect(report.logins["codex/id:gpt"]?.amount.estimated == true)
    }

    @Test func modelsAddUpOverEveryMachineAndAccount() {
        let report = UsageReport(answers)
        #expect(report.models.map(\.id) == ["codex/gpt-5", "claude/opus"])
        #expect(report.models.map(\.amount.turns) == [1, 3])
        #expect(report.models.map(\.amount.costUsd) == [3, 2.5])
        #expect(report.models.map(\.amount.estimated) == [true, false])
    }

    @Test func anAccountNoLongerListedCountsInTheTotalOnly() {
        let gone = MachineUsage(hostId: "desk", name: "desk", accounts: [],
                                totals: [total("removed", "opus")], failovers: [failover("removed", hits: 1)])
        let report = UsageReport([gone])
        #expect(report.total.turns == 1)
        #expect(report.logins.isEmpty)
    }

    @Test func amountsReadCompactly() {
        #expect(UsageReport.tokens(950) == "950")
        #expect(UsageReport.tokens(12_340) == "12.3K")
        #expect(UsageReport.tokens(4_500_000) == "4.5M")
        #expect(UsageReport.tokens(1_200_000_000) == "1.2B")
        #expect(UsageReport.dollars(UsageAmount(costUsd: 12.345)) == "$12.35")
        #expect(UsageReport.dollars(UsageAmount(costUsd: 0.004)) == "<$0.01")
        #expect(UsageReport.dollars(UsageAmount(costUsd: 3, estimated: true)) == "≈ $3.00")
    }
}

struct ProviderLoginTests {
    @Test func aLoginShowsWhatIsLeftOfItsPlanWindowsAsLastReported() throws {
        let reset = { (hours: Double) in now.addingTimeInterval(hours * 3600).ISO8601Format() }
        // desk's report of the weekly window is older, from before it reset.
        let machines = [
            machine("desk", name: "desk", sessions: [], accounts: [
                account("main", email: "dev@example.com", windows: [
                    UsageWindow(window: "seven_day", usedPercent: 95, resetsAt: reset(-1)),
                ]),
            ]),
            machine("laptop", name: "laptop", sessions: [], accounts: [
                account("work", email: "dev@example.com", windows: [
                    UsageWindow(window: "five_hour", usedPercent: 40, resetsAt: reset(2)),
                    UsageWindow(window: "seven_day", usedPercent: 10, resetsAt: reset(160)),
                ]),
                account("spare"),
            ]),
        ]
        let logins = ProviderAccounts(machines: machines, now: now).groups.flatMap(\.logins)
        let login = try #require(logins.first { $0.id == "claude/dev@example.com" })
        #expect(login.session == WindowLeft(percentUsed: 40, resets: "2h 0m"))
        #expect(login.session?.percentLeft == 60)
        #expect(login.weekly?.percentUsed == 10)
        let spare = try #require(logins.first { $0.id == "claude/id:spare" })
        #expect(spare.session == nil && spare.weekly == nil)
    }

    @Test func aLoginCountsTheOpenSessionsOfItsAccountsOnEveryMachine() throws {
        let machines = [
            machine("desk", name: "desk", sessions: [], accounts: [account("main", email: "dev@example.com"),
                                                                   account("spare", email: "dev@example.com")]),
            machine("laptop", name: "laptop", sessions: [], accounts: [account("main"), account("work", email: "dev@example.com")]),
        ]
        let login = try #require(ProviderAccounts(machines: machines).groups[0].logins
            .first { $0.id == "claude/dev@example.com" })
        let summary = { (hostId: HostId, sessions: [AccountId: Int]) in
            MachineSummary(hostId: hostId, name: hostId, connection: .connected, role: .owner, cpu: nil, memory: nil,
                           running: 0, sessions: 0,
                           accounts: sessions.map { AccountSummary(accountId: $0.key, label: $0.key, provider: "claude",
                                                                   sessions: $0.value, usage: []) },
                           hosts: [], pinned: false)
        }
        // The laptop's email-less `main` is another login.
        #expect(login.sessions(in: [summary("desk", ["main": 2, "spare": 1]), summary("laptop", ["main": 5, "work": 1])]) == 4)
    }
}

struct UsageCacheTests {
    private func answer(at: Date, asked: [HostId] = ["desk"]) -> UsageCache.Answer {
        UsageCache.Answer(machines: [], failures: [], asked: asked, at: at)
    }

    @Test func aPeriodWithoutAnAnswerIsAskedOnceAtATime() {
        var cache = UsageCache()
        #expect(cache.isStale(.week, machines: ["desk"], now: now))
        cache.asks(.week)
        #expect(!cache.isStale(.week, machines: ["desk"], now: now))
        // Another period is its own question.
        #expect(cache.isStale(.day, machines: ["desk"], now: now))
    }

    @Test func anAnswerIsShownUntilAMinuteOld() {
        var cache = UsageCache()
        cache.asks(.week)
        cache.answered(.week, with: answer(at: now))
        #expect(cache[.week] == answer(at: now))
        #expect(!cache.isStale(.week, machines: ["desk"], now: now.addingTimeInterval(59)))
        #expect(cache.isStale(.week, machines: ["desk"], now: now.addingTimeInterval(60)))
    }

    @Test func anotherSetOfMachinesMakesAnAnswerStale() {
        var cache = UsageCache()
        cache.answered(.week, with: answer(at: now))
        #expect(cache.isStale(.week, machines: ["desk", "laptop"], now: now))
        #expect(cache.isStale(.week, machines: [], now: now))
    }
}

/// Usage against fake daemons, whose hello turn reports usage
/// (`crates/herder-ffi/fixtures/hello.jsonl`).
@MainActor
struct UsageFleetTests {
    /// What one hello turn reports.
    private static let turn = UsageAmount(turns: 1, input: 1_200, output: 340, cacheRead: 18_000, cacheWrite: 2_048,
                                          costUsd: 0.0425)

    /// Opens a profile, follows it, and pairs it with `daemons`.
    private func follow(pairing daemons: [FakeDaemon], member: Bool = false) async throws -> (Fleet, Task<Void, Never>) {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CocoaError(.fileWriteUnknown)
        }
        let following = Task { await fleet.follow() }
        for daemon in daemons {
            let machine = try await fleet.pair(daemon, member: member)
            try await fleet.client.synced(hostId: machine.hostId)
        }
        return (fleet, following)
    }

    /// Runs one hello turn on `machine`, on a model of its own name.
    private func helloTurn(on machine: HostId, of daemon: FakeDaemon, fleet: Fleet) async throws {
        _ = try await fleet.createSession(
            on: machine, repo: daemon.repo, projectId: nil, accountId: daemon.account, model: "demo-model",
            mode: .ask, prompt: "Say hello.")
    }

    /// The machines' answers once they count `turns` turns.
    private func usage(_ fleet: Fleet, turns: UInt64) async -> [MachineUsage] {
        for _ in 0..<100 {
            let (machines, failures) = await fleet.usage(over: .day)
            #expect(failures.isEmpty)
            if UsageReport(machines).total.turns >= turns { return machines }
            try? await Task.sleep(for: .milliseconds(100))
        }
        Issue.record("the machines did not count \(turns) turns")
        return []
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func twoMachinesAddUpPerLogin() async throws {
        let a = try FakeDaemon(name: "usage-a")
        let b = try FakeDaemon(name: "usage-b")
        let (fleet, following) = try await follow(pairing: [a, b])
        defer { following.cancel() }
        try await helloTurn(on: "usage-a", of: a, fleet: fleet)
        try await helloTurn(on: "usage-b", of: b, fleet: fleet)

        let machines = await usage(fleet, turns: 2)
        let report = UsageReport(machines)
        var both = Self.turn
        both.turns = 2
        (both.input, both.output, both.cacheRead, both.cacheWrite) = (2_400, 680, 36_000, 4_096)
        both.costUsd = Self.turn.costUsd * 2
        #expect(report.total == both)
        // The model's turns from both in one row; each machine's login has its own turn.
        #expect(report.models.map(\.model) == ["demo-model"])
        #expect(report.models.first?.amount == both)
        let logins = ProviderAccounts(machines: fleet.machines).groups.flatMap(\.logins)
        #expect(logins.map { report.logins[$0.id]?.amount.turns ?? 0 }.reduce(0, +) == 2)
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aFreshAnswerIsKeptInsteadOfAskingAgain() async throws {
        let daemon = try FakeDaemon(name: "usage-cached")
        let (fleet, following) = try await follow(pairing: [daemon])
        defer { following.cancel() }
        await fleet.refreshUsage(over: .day)
        let first = try #require(fleet.usageCache[.day])
        #expect(first.asked == ["usage-cached"])
        #expect(first.failures.isEmpty)
        await fleet.refreshUsage(over: .day)
        #expect(fleet.usageCache[.day]?.at == first.at)
        await fleet.refreshUsage(over: .day, force: true)
        #expect(try #require(fleet.usageCache[.day]?.at) > first.at)
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aMemberSeesTheUsageOfTheSessionsTheyCanSee() async throws {
        let daemon = try FakeDaemon(name: "usage-shared")
        let (owner, following) = try await follow(pairing: [daemon])
        defer { following.cancel() }
        try await helloTurn(on: "usage-shared", of: daemon, fleet: owner)
        let ownerSees = UsageReport(await usage(owner, turns: 1))
        #expect(ownerSees.total == Self.turn)
        #expect(ownerSees.logins.values.map(\.amount) == [Self.turn])

        // Members see every session on the machine, so they get the same totals.
        let (member, memberFollowing) = try await follow(pairing: [daemon], member: true)
        defer { memberFollowing.cancel() }
        #expect(await eventually { member.machines.first?.role == .member })
        // Its usage joins a login once the machine has listed its accounts.
        #expect(await eventually { member.machines.first?.accounts.isEmpty == false })
        let memberSees = UsageReport(await usage(member, turns: 1))
        #expect(memberSees == ownerSees)
    }
}

struct AnsweredWithinTests {
    @Test func anAnswerInTimeIsReturned() async throws {
        #expect(try await answered(within: .seconds(5), or: "late") { 42 } == 42)
    }

    /// A machine that never answers, like a daemon older than the command, fails once the
    /// limit passes instead of holding the caller forever.
    @Test func noAnswerFailsOnceTheLimitPasses() async {
        let started = ContinuousClock.now
        await #expect(throws: HerderError.Local(detail: "late")) {
            try await answered(within: .milliseconds(100), or: "late") {
                try await Task.sleep(for: .seconds(60))
                return 42
            }
        }
        #expect(ContinuousClock.now - started < .seconds(5))
    }
}
