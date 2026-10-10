import Foundation
import Herder
@testable import HerderKit
import Testing

struct ProvidersTests {
    private let now = Date(timeIntervalSince1970: 1_800_000_000)

    private func account(_ id: AccountId, email: String, used: [String: Double] = [:]) -> Account {
        let resets = ISO8601DateFormatter().string(from: now.addingTimeInterval(3_600))
        return Account(accountId: id, provider: "claude", label: id, configDir: nil, email: email,
                       usage: used.map { UsageWindow(window: $0.key, usedPercent: $0.value, resetsAt: resets) },
                       fallback: false)
    }

    @Test func headroomIsPlentyUntilThirtyPercentLeftAndNearlyOutAtTen() {
        #expect(Headroom(percentLeft: 100) == .plenty)
        #expect(Headroom(percentLeft: 31) == .plenty)
        #expect(Headroom(percentLeft: 30) == .low)
        #expect(Headroom(percentLeft: 11) == .low)
        #expect(Headroom(percentLeft: 10) == .nearlyOut)
        #expect(WindowLeft(percentUsed: 120, resets: "").headroom == .nearlyOut)
        #expect(Headroom.nearlyOut < .low && Headroom.low < .plenty)
    }

    @Test func theTightestWindowIsTheHeadlineAndLoginsNeedingAttentionComeFirst() {
        let machines = [
            machine("a", name: "laptop", sessions: [], accounts: [
                account("calm", email: "a@example.com", used: ["five_hour": 10, "seven_day": 20]),
                account("tight", email: "b@example.com", used: ["five_hour": 5, "seven_day": 92]),
                account("low", email: "c@example.com", used: ["five_hour": 75]),
                account("quiet", email: "d@example.com"),
            ]),
            machine("b", name: "server", sessions: [], accounts: [
                account("calm", email: "a@example.com", used: ["five_hour": 10, "seven_day": 20]),
                account("tight", email: "b@example.com", used: ["five_hour": 5, "seven_day": 92]),
                account("low", email: "c@example.com", used: ["five_hour": 75]),
                account("quiet", email: "d@example.com"),
            ]),
        ]
        let logins = ProviderAccounts(machines: machines, now: now).groups[0].logins
        #expect(logins.map(\.email) == ["b@example.com", "c@example.com", "a@example.com", "d@example.com"])
        let tight = logins[0]
        #expect(tight.tightest?.label == "Weekly")
        #expect(tight.tightest?.window.percentLeft == 8)
        #expect(tight.headroom == .nearlyOut)
        #expect(tight.needsAttention)
        #expect(logins[1].headroom == .low)
        #expect(!logins[2].needsAttention)
        #expect(logins[2].tightest?.label == "Weekly")
        // A login that reports no windows has plenty, as far as anyone knows.
        #expect(logins[3].tightest == nil && logins[3].headroom == .plenty && !logins[3].needsAttention)
    }

    @Test func aLoginMissingOnAMachineNeedsAttentionWithRoomToSpare() {
        let machines = [
            machine("a", name: "laptop", sessions: [], accounts: [
                account("here", email: "a@example.com", used: ["five_hour": 1]),
                account("both", email: "b@example.com", used: ["five_hour": 50]),
            ]),
            machine("b", name: "server", sessions: [], accounts: [
                account("both", email: "b@example.com", used: ["five_hour": 50]),
            ]),
        ]
        let logins = ProviderAccounts(machines: machines, now: now).groups[0].logins
        #expect(logins.map(\.email) == ["a@example.com", "b@example.com"])
        #expect(logins[0].headroom == .plenty && logins[0].needsAttention)
    }

    @Test func accountsAreAddedOnMachinesThisDeviceOwnsAndReaches() {
        let vault = FleetHost(hostId: "a", hostName: "laptop", online: true, lastSeen: "2026-01-01T00:00:00Z")
        let machines = [
            machine("a", name: "laptop", sessions: []),
            machine("b", name: "server", sessions: [], role: .member),
            machine("c", name: "mini", sessions: [], connection: .connecting),
            machine("d", name: "vault", sessions: [], hosts: [vault]),
            machine("e", name: "never connected", sessions: [], role: nil),
        ]
        let targets = AccountTarget.all(machines)
        #expect(targets == [
            .init(hostId: "a", name: "laptop", problem: nil),
            .init(hostId: "b", name: "server", problem: "Owners only"),
            .init(hostId: "c", name: "mini", problem: "Not connected"),
        ])
        // With one machine to add on, the sheet goes straight to its login.
        #expect(AccountTarget.only(targets) == "a")
        #expect(AccountTarget.only(AccountTarget.all(machines + [machine("f", name: "desk", sessions: [])])) == nil)
        #expect(AccountTarget.only(Array(targets.dropFirst())) == nil)
    }

    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func addingAnAccountStartsTheLoginOnThePickedMachine() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let machine = try await fleet.pair(daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        #expect(await eventually { AccountTarget.only(AccountTarget.all(fleet.machines)) != nil })
        let hostId = try #require(AccountTarget.only(AccountTarget.all(fleet.machines)))
        #expect(hostId == machine.hostId)
        var draft = AccountDraft()
        draft.provider = "codex"
        draft.fillId(taken: fleet.machines.first { $0.hostId == hostId }?.accounts.map(\.accountId) ?? [])
        #expect(draft.problem(existing: []) == nil)
        // The fake daemon cannot log its fake provider in, and says so: the login reached it.
        let connection = TerminalConnection(hostId: hostId, terminalId: nil, account: draft.account)
        connection.connect(client: fleet.client, sessionId: nil, cols: 80, rows: 24)
        #expect(await eventually {
            if case .failed = connection.state { return true }
            return false
        })
    }
}
