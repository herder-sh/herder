import Foundation
import Herder
@testable import HerderKit
import Testing

struct ModelCatalogTests {
    @Test func groupsFollowTheKnownProvidersWithUsedModelsAfterTheCatalogs() {
        let groups = ModelCatalog.groups(
            providers: ["zed", "codex", "claude", "codex"],
            current: .init(provider: "claude", model: "claude-opus-4-1"),
            used: ["claude": ["claude-sonnet-5-5", "claude-3-7"]],
            offersDefault: false)
        #expect(groups.map(\.provider) == ["claude", "codex", "zed"])
        #expect(groups[0].models.map(\.id) == [
            "claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1", "claude-haiku-4-5-20251001",
            "claude-3-7", "claude-opus-4-1",
        ])
        #expect(groups[1].models.map(\.id) == ["gpt-6.1-sol", "gpt-6-luna"])
        // No catalog for the provider: its default is the one choice.
        #expect(groups[2].models.map(\.id) == [""])
        #expect(groups[2].models[0].name == "Zed default")
    }

    @Test func draftsOfferEachProvidersDefault() {
        let groups = ModelCatalog.groups(
            providers: ["claude", "gemini"], current: .init(provider: "claude", model: "claude-opus-5-5"),
            used: [:], offersDefault: true)
        #expect(groups.map { $0.models.last?.id } == ["", ""])
        #expect(groups[1].models.count == 1)
    }

    @Test func theFilterMatchesEveryWordAcrossNameIdAndProvider() {
        let groups = ModelCatalog.groups(
            providers: ["claude", "codex"], current: nil, used: [:], offersDefault: true)
        #expect(ModelCatalog.filter(groups, "").map(\.provider) == ["claude", "codex"])
        let opus = ModelCatalog.filter(groups, "claude OPUS")
        #expect(opus.map(\.provider) == ["claude"])
        #expect(opus[0].models.map(\.id) == ["claude-opus-5-5"])
        #expect(ModelCatalog.filter(groups, "codex").first?.models.count == 3)
        #expect(ModelCatalog.filter(groups, "gpt-6.1").flatMap(\.models).map(\.id) == ["gpt-6.1-sol"])
        #expect(ModelCatalog.filter(groups, "nothing like it").isEmpty)
    }

    @Test func namesFallBackToTheId() {
        #expect(ModelCatalog.name("gpt-6-luna", provider: "codex") == "GPT-6 Luna")
        #expect(ModelCatalog.name("custom-1", provider: "codex") == "custom-1")
        #expect(ModelCatalog.name("", provider: "opencode") == "OpenCode default")
    }

    @Test func providerLogosParseAndUnknownOnesHaveNone() {
        for provider in ["claude", "codex", "gemini", "cursor", "opencode"] {
            let logo = ProviderLogo(provider)
            #expect(logo != nil)
            let bounds = logo?.path(in: CGRect(x: 0, y: 0, width: 24, height: 24)).boundingRect ?? .null
            #expect(bounds.width > 12 && bounds.maxX <= 24.5 && bounds.maxY <= 24.5, "\(provider): \(bounds)")
        }
        #expect(ProviderLogo("grok") == nil)
        #expect(ProviderLogo.parse("M1 2L3 4C1 2 3 4 5 6Z").map(\.0) == ["M", "L", "C", "Z"])
    }
}

struct SettingsMenuTests {
    @Test func accountsAreTheProvidersWithTheirBusiestWindow() {
        let accounts = [
            Account(accountId: "main", provider: "claude", label: "Main", configDir: nil, usage: [
                UsageWindow(window: "five_hour", usedPercent: 12.4, resetsAt: nil),
                UsageWindow(window: "seven_day", usedPercent: 61.6, resetsAt: nil),
            ]),
            Account(accountId: "gpt", provider: "codex", label: "GPT", configDir: nil, usage: []),
            Account(accountId: "work", provider: "claude", label: "Work", configDir: nil, usage: []),
        ]
        let options = SettingsOption.accounts(accounts, provider: "claude", current: "work")
        #expect(options.map(\.id) == ["main", "work"])
        #expect(options[0].detail == "Weekly 62%")
        #expect(options[0].usage == 61.6)
        #expect(options.map(\.current) == [false, true])
        // No usage reported: no detail and no meter.
        #expect(options[1].detail == nil && options[1].usage == nil)
    }

    @Test func machinesPutTheCurrentFirstAndSayWhyOthersCannotBePicked() {
        let machines = [machine("a", name: "Studio", sessions: []), machine("b", name: "Laptop", sessions: []),
                        machine("c", name: "Server", sessions: [])]
        let options = SettingsOption.machines(machines, current: "b") { $0.hostId == "c" ? "Offline" : nil }
        #expect(options.map(\.title) == ["Laptop", "Studio", "Server"])
        #expect(options.map(\.current) == [true, false, false])
        #expect(options.map(\.unavailable) == [nil, nil, "Offline"])
        #expect(options[2].detail == "Offline")
        // The current machine is never unavailable, whatever the rule says of it.
        #expect(SettingsOption.machines(machines, current: "c") { _ in "Offline" }.first?.unavailable == nil)
    }

    @Test func pickingTheCurrentOptionChangesNothing() {
        var chosen: [String] = []
        var ran = 0
        let section = SettingsSection(
            kind: .machine,
            options: [SettingsOption(id: "a", title: "A", current: true), SettingsOption(id: "b", title: "B")],
            action: .init(title: "Fork Session…", symbol: "arrow.triangle.branch") { ran += 1 }
        ) { chosen.append($0) }
        section.perform(.option(.machine, "a"))
        section.perform(.option(.machine, "b"))
        section.perform(.action(.machine))
        #expect(chosen == ["b"])
        #expect(ran == 1)
        #expect(ModelMenu.Entry.option(.account, "x").section == .account)
        #expect(ModelMenu.Entry.other.section == nil)
    }
}

@MainActor
struct ProjectIconTests {
    @Test func theTintIsStableAndSpreadsAcrossThePalette() {
        // FNV-1a, not Swift's per-launch hash: these hold on every run and device.
        #expect(ProjectIcon.tintIndex("github.com/acme/demo") == ProjectIcon.tintIndex("github.com/acme/demo"))
        let offsetBasis: UInt64 = 0xcbf2_9ce4_8422_2325
        #expect(ProjectIcon.tintIndex("") == Int(offsetBasis % UInt64(ProjectIcon.tints.count)))
        let indices = Set((0..<50).map { ProjectIcon.tintIndex("github.com/acme/project-\($0)") })
        #expect(indices.count == ProjectIcon.tints.count)
    }

    @Test func theInitialComesFromTheLastPathComponent() {
        #expect(ProjectIcon.initial("github.com/acme/demo") == "D")
        #expect(ProjectIcon.initial("host:/src/.herder") == "H")
        #expect(ProjectIcon.initial("2048") == "2")
        #expect(ProjectIcon.initial("---") == "#")
    }
}

@MainActor
struct DraftModelTests {
    private func makeFleet(_ hosts: [Machine]) -> Fleet? {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else { return nil }
        fleet.setMachinesForTesting(hosts)
        return fleet
    }

    @Test func pickingAnotherProvidersModelStartsTheSessionOnItsAccount() throws {
        var host = machine("h", name: "h", sessions: [],
                           projects: [Project(projectId: "p", name: "p", paths: [], defaultPermissionMode: nil,
                                              defaultAccount: "main", setupCommand: nil)])
        host.accounts = [
            Account(accountId: "main", provider: "claude", label: "main", configDir: nil, usage: []),
            Account(accountId: "gpt", provider: "codex", label: "gpt", configDir: nil, usage: []),
        ]
        let fleet = try #require(makeFleet([host]))
        #expect(fleet.draftChoice(on: "h", projectId: "p") == .init(provider: "claude", model: "claude-opus-5-5"))
        #expect(Set(fleet.providers(on: "h")) == ["claude", "codex"])
        let groups = fleet.modelGroups(on: "h", providers: fleet.providers(on: "h"),
                                       current: fleet.draftChoice(on: "h", projectId: "p"), offersDefault: true)
        #expect(groups.map(\.provider) == ["claude", "codex"])
        // The menu picks a Codex model: the session goes to the Codex account, not the project's.
        let picked = ModelCatalog.Choice(provider: "codex", model: groups[1].models[0].id)
        #expect(fleet.defaultAccount(on: "h", projectId: "p", provider: picked.provider)?.accountId == "gpt")
    }

    @Test func movingToAMachineWithoutTheProviderFallsBackToItsDefault() throws {
        var both = machine("a", name: "a", sessions: [])
        both.accounts = [
            Account(accountId: "main", provider: "claude", label: "main", configDir: nil, usage: []),
            Account(accountId: "gpt", provider: "codex", label: "gpt", configDir: nil, usage: []),
        ]
        var claudeOnly = machine("b", name: "b", sessions: [])
        claudeOnly.accounts = [Account(accountId: "other", provider: "claude", label: "other", configDir: nil, usage: [])]
        let fleet = try #require(makeFleet([both, claudeOnly]))
        let codex = ModelCatalog.Choice(provider: "codex", model: "gpt-6-luna")
        #expect(fleet.draftChoice(codex, movedTo: "a", projectId: nil) == codex)
        #expect(fleet.draftChoice(codex, movedTo: "b", projectId: nil) == .init(provider: "claude", model: "claude-opus-5-5"))
        let sonnet = ModelCatalog.Choice(provider: "claude", model: "claude-sonnet-5-5")
        #expect(fleet.draftChoice(sonnet, movedTo: "b", projectId: nil) == sonnet)
    }
}
