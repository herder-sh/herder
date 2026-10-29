import Foundation
import Herder
@testable import HerderKit
import Testing

struct MachineAttentionTests {
    private func account(_ id: String, needsLogin: String? = nil) -> Account {
        Account(accountId: id, provider: "claude", label: id.capitalized, configDir: nil, email: nil, usage: [],
                needsLogin: needsLogin)
    }

    private func status(_ provider: Provider, _ version: String, latest: String? = nil) -> ProviderStatus {
        ProviderStatus(provider: provider, installed: true, version: version, latestVersion: latest, binary: nil,
                       canInstall: true, canUpdate: true)
    }

    @Test func listsSignedOutAccountsWithTheSessionsTheyHoldUp() throws {
        var devbox = machine("a", name: "devbox", sessions: ["01A", "01B", "01C"],
                             accounts: [account("work"), account("ops", needsLogin: "2026-10-26T10:00:00Z")])
        devbox.sessions[0].accountId = "ops"
        devbox.sessions[0].status = .needsLogin
        devbox.sessions[1].accountId = "ops"
        devbox.sessions[1].status = .needsLogin
        devbox.sessions[2].accountId = "work"
        let attention = MachineAttention(machine: devbox, versions: ProviderVersions(machines: [devbox]))
        #expect(attention.signedOut.map(\.id) == ["ops"])
        #expect(attention.signedOut[0].waiting == 2)
        let now = try #require(Timestamp.date("2026-10-26T10:14:00Z"))
        #expect(MachineAttention.detail(attention.signedOut[0], now: now) == "2 sessions waiting · since 14m ago")
        #expect(!attention.isEmpty)
    }

    @Test func listsTheUpdatesHerderCanRunOnThisMachineOnly() {
        let devbox = machine("a", name: "devbox", sessions: [], providers: [
            status("claude", "2.1.9 (Claude Code)", latest: "2.1.20"),
            status("codex", "codex-cli 0.44.0"),
        ])
        let studio = machine("b", name: "studio", sessions: [], providers: [
            status("claude", "2.1.20 (Claude Code)"),
            status("codex", "codex-cli 0.46.0"),
        ])
        let versions = ProviderVersions(machines: [devbox, studio])
        let attention = MachineAttention(machine: devbox, versions: versions)
        #expect(attention.updates.map(\.provider) == ["claude", "codex"])
        #expect(attention.updatesTitle == "2 CLI updates")
        #expect(attention.updatesDetail == "Claude 2.1.9 → 2.1.20 · Codex 0.44.0 → 0.46.0")
        #expect(attention.updateTitle == "Update both")
        #expect(MachineAttention(machine: studio, versions: versions).isEmpty)
    }

    @Test func aMemberHasNoUpdatesToRun() {
        let devbox = machine("a", name: "devbox", sessions: [],
                             providers: [status("claude", "2.1.9 (Claude Code)", latest: "2.1.20")], role: .member)
        #expect(MachineAttention(machine: devbox, versions: ProviderVersions(machines: [devbox])).isEmpty)
    }
}

struct CapacityTests {
    @Test func fitsACoreAndTwoGigabytesPerTurn() {
        // 8 cores keep 2 for the rest; 16 GB would fit 8.
        #expect(TurnLoad.fit(cores: 8, memoryBytes: 16 << 30) == 6)
        // 32 cores would fit 24; 36 GB fits 18.
        #expect(TurnLoad.fit(cores: 32, memoryBytes: 36 << 30) == 18)
        #expect(TurnLoad.fit(cores: 1, memoryBytes: 1 << 30) == 1)
    }

    @Test func hintsAtTheMachineAndWhatFits() {
        let resources = HostResources(cpuCores: 8, cpuPercent: 0, load1m: 0, memoryTotalBytes: 16 << 30,
                                      memoryAvailableBytes: 8 << 30, pressure: nil, runningTurns: 2, maxTurns: 4,
                                      waitingTurns: 0, constraint: nil)
        #expect(TurnLoad.fitHint(resources) == "16 GB, 8 cores · about 6 fit")
    }
}

struct BackupFreshnessTests {
    private func replication(last: String?, lagMs: UInt64?) -> HostReplication {
        HostReplication(hostId: "a", sessions: 1, events: 10, lastEventAt: last, lagMs: lagMs)
    }

    @Test func saysHowFarTheVaultGot() throws {
        let now = try #require(Timestamp.date("2026-10-26T10:03:00Z"))
        #expect(Fleet.freshness(nil, now: now) == "")
        #expect(Fleet.freshness(replication(last: nil, lagMs: nil), now: now) == "Nothing backed up yet")
        #expect(Fleet.freshness(replication(last: "2026-10-26T10:00:00Z", lagMs: 400), now: now) == "Caught up · 3m ago")
        #expect(Fleet.freshness(replication(last: "2026-10-26T10:00:00Z", lagMs: 600_000), now: now)
            == "Last change held 3m ago")
        #expect(Fleet.freshness(replication(last: "2026-10-26T10:00:00Z", lagMs: nil), now: now)
            == "Last change held 3m ago")
    }
}
