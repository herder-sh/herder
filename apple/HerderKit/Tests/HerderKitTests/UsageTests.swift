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

private func account(_ id: AccountId, provider: Provider = "claude", windows: [UsageWindow] = []) -> Account {
    Account(accountId: id, provider: provider, label: id.capitalized, configDir: nil, usage: windows)
}

struct UsageReportTests {
    /// Two machines, each with a Claude account named `work`; the laptop's also ran Codex.
    private let machines = [
        MachineUsage(hostId: "desk", name: "desk", accounts: [
            account("work", windows: [
                UsageWindow(window: "five_hour", usedPercent: 40, resetsAt: now.addingTimeInterval(7200).ISO8601Format()),
                UsageWindow(window: "seven_day", usedPercent: 75, resetsAt: nil),
            ]),
            account("idle"),
        ], totals: [total("work", "opus", turns: 2, input: 200, cost: 2)]),
        MachineUsage(hostId: "laptop", name: "laptop", accounts: [account("work"), account("gpt", provider: "codex")],
                     totals: [
                         total("work", "opus", cost: 0.5),
                         total("gpt", "gpt-5", provider: "codex", cost: 3, estimated: true),
                     ]),
    ]

    @Test func allMachinesAddUpOverallPerAccountAndPerModel() {
        let report = UsageReport(machines, now: now)
        #expect(report.total == UsageAmount(turns: 4, input: 400, output: 30, cacheRead: 900, cacheWrite: 0,
                                            costUsd: 5.5, estimated: true))
        // Each machine's account is its own row, the costliest first; one with no turns and
        // no plan window is left out.
        #expect(report.accounts.map(\.id) == ["laptop/gpt", "desk/work", "laptop/work"])
        #expect(report.accounts.map(\.amount.turns) == [1, 2, 1])
        #expect(report.accounts.map(\.machine) == ["laptop", "desk", "laptop"])
        // A model's turns on every machine and account add up into one row.
        #expect(report.models.map(\.id) == ["codex/gpt-5", "claude/opus"])
        #expect(report.models.map(\.amount.turns) == [1, 3])
        #expect(report.models.map(\.amount.costUsd) == [3, 2.5])
        #expect(report.models.map(\.amount.estimated) == [true, false])
    }

    @Test func oneMachineShowsOnlyItsOwn() {
        let report = UsageReport(machines, only: "desk", now: now)
        #expect(report.total.turns == 2)
        #expect(report.total.costUsd == 2)
        #expect(!report.total.estimated)
        #expect(report.accounts.map(\.id) == ["desk/work"])
        #expect(report.models.map(\.id) == ["claude/opus"])
    }

    @Test func anAccountShowsWhatIsLeftOfItsPlanWindows() throws {
        let work = try #require(UsageReport(machines, now: now).accounts.first { $0.id == "desk/work" })
        #expect(work.session == WindowLeft(percentUsed: 40, resets: "2h 0m"))
        #expect(work.session?.percentLeft == 60)
        #expect(work.weekly == WindowLeft(percentUsed: 75, resets: ""))
        // An account with a plan window but no turns still shows, for its window.
        let windowed = MachineUsage(hostId: "desk", name: "desk", accounts: [
            account("spare", windows: [UsageWindow(window: "five_hour", usedPercent: 100, resetsAt: nil)]),
        ], totals: [])
        let spare = try #require(UsageReport([windowed], now: now).accounts.first)
        #expect(spare.amount.turns == 0)
        #expect(spare.session?.percentLeft == 0)
    }

    @Test func usageOfAnAccountNoLongerListedKeepsItsRow() {
        let gone = MachineUsage(hostId: "desk", name: "desk", accounts: [],
                                totals: [total("removed", "opus")])
        let report = UsageReport([gone], now: now)
        #expect(report.accounts.map(\.label) == ["removed"])
        #expect(report.total.turns == 1)
    }

    @Test func cacheSavingsAreTheShareOfInputReadFromTheCache() {
        #expect(UsageAmount().cacheSavings == nil)
        #expect(UsageAmount(input: 100, cacheRead: 300).cacheSavings == 0.75)
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

/// The Usage screen against fake daemons, whose hello turn reports usage
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
    func twoMachinesAddUpAndEachCanBeShownAlone() async throws {
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
        // One row per machine's account, and the model's turns from both in one row.
        #expect(Set(report.accounts.map(\.id)) == ["usage-a/\(a.account)", "usage-b/\(b.account)"])
        #expect(report.models.map(\.model) == ["demo-model"])
        #expect(report.models.first?.amount == both)

        let alone = UsageReport(machines, only: "usage-b")
        #expect(alone.total == Self.turn)
        #expect(alone.accounts.map(\.machine) == ["usage-b"])
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aMemberSeesTheUsageOfTheSessionsTheyCanSee() async throws {
        let daemon = try FakeDaemon(name: "usage-shared")
        let (owner, following) = try await follow(pairing: [daemon])
        defer { following.cancel() }
        try await helloTurn(on: "usage-shared", of: daemon, fleet: owner)
        let ownerSees = UsageReport(await usage(owner, turns: 1))
        #expect(ownerSees.total == Self.turn)

        // Members see every session on the machine, so they get the same totals.
        let (member, memberFollowing) = try await follow(pairing: [daemon], member: true)
        defer { memberFollowing.cancel() }
        #expect(await eventually { member.machines.first?.role == .member })
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
