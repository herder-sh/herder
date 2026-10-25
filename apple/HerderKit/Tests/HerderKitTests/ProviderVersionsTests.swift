import Herder
@testable import HerderKit
import Testing

struct ProviderVersionsTests {
    private func status(_ provider: Provider, _ version: String?, latest: String? = nil, canInstall: Bool = true,
                        canUpdate: Bool = true) -> ProviderStatus {
        ProviderStatus(provider: provider, installed: version != nil, version: version, latestVersion: latest,
                       binary: nil, canInstall: canInstall, canUpdate: version != nil && canUpdate)
    }

    @Test func aNewerVendorReleaseMarksEveryMachineBehindIt() {
        // devbox has the newest any machine has, but Claude Code's vendor publishes a newer one.
        let devbox = machine("a", name: "devbox", sessions: [], providers: [
            status("claude", "2.1.14 (Claude Code)", latest: "2.1.20"),
        ])
        // studio's check has not answered yet: it is behind devbox, and gets the vendor's release.
        let studio = machine("b", name: "studio", sessions: [], providers: [status("claude", "2.1.9 (Claude Code)")])
        let laptop = machine("c", name: "laptop", sessions: [], providers: [
            status("claude", "2.1.9 (Claude Code)", latest: "2.1.20"),
        ])
        let claude = ProviderVersions(machines: [devbox, studio, laptop]).rows[0]
        #expect(claude.latest == "2.1.20")
        #expect(claude.cells.map(\.state) == [.outdated, .outdated, .outdated])
        #expect(claude.cells.map(\.available) == ["2.1.20", "2.1.14", "2.1.20"])
        #expect(claude.cells[0].action == .update)
    }

    @Test func noVendorReleaseLeavesTheNewestMachineCurrent() {
        let devbox = machine("a", name: "devbox", sessions: [], providers: [status("claude", "2.1.14")])
        let studio = machine("b", name: "studio", sessions: [], providers: [status("claude", "2.1.9")])
        let claude = ProviderVersions(machines: [devbox, studio]).rows[0]
        #expect(claude.latest == "2.1.14")
        #expect(claude.cells.map(\.state) == [.current, .outdated])
        #expect(claude.cells.map(\.available) == [nil, "2.1.14"])
    }

    @Test func theLatestIsTheVendorsReleaseWhenOneMachineIsBehindIt() {
        // work-mbp is current; trash-can-01 is behind and reports the release work-mbp has.
        let work = machine("a", name: "work-mbp", sessions: [], providers: [status("claude", "2.1.310", latest: "2.1.310")])
        let trash = machine("b", name: "trash-can-01", sessions: [], providers: [status("claude", "2.1.303", latest: "2.1.310")])
        let claude = ProviderVersions(machines: [work, trash]).rows[0]
        #expect(claude.latest == "2.1.310")
        #expect(claude.cells.map(\.state) == [.current, .outdated])
        #expect(claude.outdated.map(\.machine) == ["trash-can-01"])
    }

    @Test func eachMachineIsComparedWithTheNewestAnyHas() {
        let devbox = machine("a", name: "devbox", sessions: [], providers: [
            status("claude", "2.1.14 (Claude Code)"), status("codex", "codex-cli 0.46.0"),
        ])
        let studio = machine("b", name: "studio", sessions: [], providers: [
            status("claude", "2.1.9 (Claude Code)"), status("codex", nil),
        ])
        let laptop = machine("c", name: "laptop", sessions: [], providers: [
            status("claude", "2.1.9 (Claude Code)", canUpdate: false), status("codex", nil, canInstall: false),
        ])
        let versions = ProviderVersions(machines: [devbox, studio, laptop])
        #expect(versions.rows.map(\.provider) == ["claude", "codex"])
        let claude = versions.rows[0]
        #expect(claude.latest == "2.1.14")
        #expect(claude.cells.map(\.state) == [.current, .outdated, .outdated])
        #expect(claude.cells.map(\.version) == ["2.1.14", "2.1.9", "2.1.9"])
        #expect(claude.cells[1].action == .update && claude.cells[1].blocked == nil)
        #expect(claude.cells[2].blocked?.contains("no updater") == true)
        let codex = versions.rows[1]
        #expect(codex.cells.map(\.state) == [.current, .missing, .missing])
        #expect(codex.cells[1].action == .install && codex.cells[1].blocked == nil)
        #expect(codex.cells[2].blocked?.contains("no installer") == true)
        // Update All runs only what herder can run.
        #expect(versions.updatable.map(\.hostId) == ["b"])
    }

    @Test func membersAndDisconnectedMachinesCannotInstall() {
        let devbox = machine("a", name: "devbox", sessions: [], providers: [status("claude", "2.0.0")])
        let member = machine("b", name: "shared", sessions: [], providers: [status("claude", "1.0.0")], role: .member)
        let away = machine("c", name: "away", sessions: [], providers: [status("claude", "1.0.0")],
                           connection: .disconnected(error: "gone"))
        let cells = ProviderVersions(machines: [devbox, member, away]).rows[0].cells
        #expect(cells[1].blocked?.contains("owner") == true)
        #expect(cells[2].blocked?.contains("not connected") == true)
        #expect(ProviderVersions(machines: [devbox, member, away]).updatable.isEmpty)
    }

    @Test func aProviderShowsOnceInstalledOrUsedSomewhere() {
        var laptop = machine("a", name: "laptop", sessions: [], providers: [status("grok", nil), status("cursor", nil)])
        laptop.accounts = [Account(accountId: "cursor", provider: "cursor", label: "Cursor", configDir: nil, email: nil, usage: [])]
        let versions = ProviderVersions(machines: [laptop])
        #expect(versions.rows.map(\.provider) == ["cursor"])
        #expect(versions.rows[0].latest == nil)
        #expect(versions.rows[0].cells[0].state == .missing)
    }

    @Test func versionNumbersComeOutOfTheCliLine() {
        #expect(ProviderVersions.number("2.1.14 (Claude Code)") == "2.1.14")
        #expect(ProviderVersions.number("codex-cli 0.46.0") == "0.46.0")
        #expect(ProviderVersions.number("2025.10.02-7a1b2c") == "2025.10.02")
        #expect(ProviderVersions.number("nightly") == "nightly")
    }

    @Test func versionCompareIsLexicographicOnDottedNumbers() {
        #expect(ProviderVersions.version("2.1.0", isNewerThan: "1.9.9"))
        #expect(ProviderVersions.version("1.2.1", isNewerThan: "1.2"))
        #expect(!ProviderVersions.version("1.2.0", isNewerThan: "1.2.0"))
        #expect(!ProviderVersions.version("1.0", isNewerThan: "1.0.1"))
        #expect(ProviderVersions.version("v2.0.0", isNewerThan: "1.9"))
        #expect(ProviderVersions.version("2.1.14", isNewerThan: "2.1.9"))
    }

    @Test func oneBannerCountsEveryLoginMissingWhereYouOwnTheMachine() {
        let account = { (id: String, provider: Provider) in
            Account(accountId: id, provider: provider, label: id, configDir: nil, email: nil, usage: [])
        }
        let devbox = machine("a", name: "devbox", sessions: [], accounts: [account("claude", "claude"), account("codex", "codex")])
        let studio = machine("b", name: "studio", sessions: [], accounts: [account("claude", "claude")])
        let laptop = machine("c", name: "laptop", sessions: [])
        let shared = machine("d", name: "shared", sessions: [], role: .member)
        let gaps = SetUpGaps(ProviderAccounts(machines: [devbox, studio, laptop, shared]))
        #expect(gaps.gaps.count == 2)
        #expect(gaps.gaps.allSatisfy { !$0.machines.map(\.name).contains("shared") })
        #expect(gaps.title == "2 accounts aren’t on every machine")
        #expect(gaps.detail.hasPrefix("Set them up on laptop and studio") || gaps.detail.hasPrefix("Set them up on studio and laptop"))
        #expect(SetUpGaps(ProviderAccounts(machines: [devbox])).isEmpty)
    }
}
