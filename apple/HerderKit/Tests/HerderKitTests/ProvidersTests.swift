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
        let targets = AccountTarget.all(machines, provider: "claude")
        #expect(targets == [
            .init(hostId: "a", name: "laptop", problem: nil),
            .init(hostId: "b", name: "server", problem: "Member — can’t add accounts"),
            .init(hostId: "c", name: "mini", problem: "Not connected"),
        ])
        // A machine without the provider's CLI offers to install it, where herder can.
        let missing = { (canInstall: Bool) in
            ProviderStatus(provider: "cursor", installed: false, version: nil, binary: nil, canInstall: canInstall,
                           canUpdate: false)
        }
        let bare = [machine("f", name: "desk", sessions: [], providers: [missing(true)]),
                    machine("g", name: "nas", sessions: [], providers: [missing(false)])]
        #expect(AccountTarget.all(bare, provider: "cursor") == [
            .init(hostId: "f", name: "desk", problem: "Cursor isn’t installed", installable: true),
            .init(hostId: "g", name: "nas", problem: "Cursor isn’t installed"),
        ])
        #expect(AccountTarget.all(bare, provider: "claude").allSatisfy { $0.problem == nil })
    }
}
