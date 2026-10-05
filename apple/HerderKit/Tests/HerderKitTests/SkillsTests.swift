import Foundation
import Herder
@testable import HerderKit
import Testing

private func status(
    repo: String? = "git@example.com:you/skills.git", head: String? = "0123456789abcdef", error: String? = nil,
    skills: [LibrarySkill] = []
) -> SkillsStatus {
    SkillsStatus(repo: repo, head: head, lastPull: "2026-01-01T04:00:00Z", pullError: error, skills: skills,
                 reload: [ProviderReload(provider: "claude", reload: .live)])
}

private func skill(_ name: String, enabled: Bool = true, providers: [Provider] = ["claude"]) -> LibrarySkill {
    LibrarySkill(name: name, description: "Does \(name).", enabled: enabled, providers: enabled ? providers : [])
}

struct SkillLibraryTests {
    @Test func skillsMergeAcrossMachinesWithAStatePerMachine() {
        var desk = machine("desk", name: "desk", sessions: [])
        desk.skills = status(skills: [skill("review-pr"), skill("deploy", providers: ["claude", "codex"])])
        var laptop = machine("laptop", name: "laptop", sessions: [])
        laptop.skills = status(skills: [skill("review-pr", enabled: false)])
        let library = SkillLibrary([desk, laptop])

        #expect(library.skills.map(\.name) == ["deploy", "review-pr"])
        let review = library.skills[1]
        #expect(review.machines.map(\.hostId) == ["desk", "laptop"])
        #expect(review.machines.map(\.enabled) == [true, false])
        #expect(review.machines.allSatisfy { $0.changeable })
        // A disabled skill reaches no CLI on its machine; the providers are every machine's.
        #expect(review.providers == ["claude"])
        #expect(library.skills[0].providers == ["claude", "codex"])
        #expect(library.repo == "git@example.com:you/skills.git")
        #expect(library.writer == "desk")
        #expect(library.pullable == ["desk", "laptop"])
    }

    @Test func eachMachineShowsItsCheckout() throws {
        var desk = machine("desk", name: "desk", sessions: [])
        desk.skills = status(error: "cannot fetch: connection refused")
        var fresh = machine("fresh", name: "fresh", sessions: [])
        fresh.skills = status(repo: nil, head: nil)
        let silent = machine("silent", name: "silent", sessions: [])
        let library = SkillLibrary([desk, fresh, silent])

        let checkout = try #require(library.checkouts.first)
        #expect(checkout.head.map(SkillLibrary.shortHead) == "0123456")
        #expect(checkout.lastPull == Timestamp.date("2026-01-01T04:00:00Z"))
        #expect(checkout.pullError == "cannot fetch: connection refused")
        #expect(library.checkouts.map(\.reported) == [true, true, false])
        #expect(library.checkouts.map(\.repo) == ["git@example.com:you/skills.git", nil, nil])
        // Writes and pulls go only through a machine with the library.
        #expect(library.writer == "desk")
        #expect(library.pullable == ["desk"])
    }

    @Test func aMemberSeesTheSkillsButChangesNothing() {
        var shared = machine("shared", name: "shared", sessions: [])
        shared.role = .member
        shared.skills = status(skills: [skill("review-pr")])
        let library = SkillLibrary([shared])
        #expect(library.skills.map(\.name) == ["review-pr"])
        #expect(library.writer == nil)
        #expect(library.setter == nil)
        #expect(library.pullable.isEmpty)
        #expect(library.skills[0].machines.map(\.changeable) == [false])
    }

    @Test func aVaultKeepsNoLibrary() {
        let vault = machine("vault", name: "vault", sessions: [],
                            hosts: [FleetHost(hostId: "desk", hostName: "desk", online: true, lastSeen: "")])
        #expect(SkillLibrary([vault]).checkouts.isEmpty)
    }

    @Test func projectSkillsAreEachProjectsOnce() {
        var desk = machine("desk", name: "desk", sessions: ["a", "b", "c"],
                           projects: [Project(projectId: "web", name: "Web", paths: [], defaultPermissionMode: nil,
                                              defaultAccount: nil, setupCommand: nil)])
        desk.sessions[0].projectId = "web"
        desk.sessions[1].projectId = "web"
        let deploy = SessionSkill(name: "deploy", description: "Deploy.", source: .project,
                                  path: ".claude/skills/deploy")
        let library = SessionSkill(name: "review-pr", description: "Review.", source: .library, path: nil)
        desk.sessionSkills = ["a": [library, deploy], "b": [deploy], "c": [library]]
        #expect(ProjectSkills.all([desk]) == [ProjectSkills(projectId: "web", name: "Web", skills: [deploy])])
    }
}

struct SkillDocumentTests {
    @Test func namesFollowTheAgentSkillsFormat() {
        #expect(SkillDocument.isValidName("review-pr"))
        #expect(SkillDocument.isValidName("v2"))
        for bad in ["", "Review", "-pr", "pr-", "re--view", "re view", "rév", String(repeating: "a", count: 65)] {
            #expect(!SkillDocument.isValidName(bad), "\(bad)")
        }
    }

    @Test func theSkillMdHasFrontMatterThenTheBody() {
        let document = SkillDocument(name: "review-pr", description: "Review a PR.\nCarefully.",
                                     body: "\n1. Read the diff.\n")
        #expect(document.markdown == "---\nname: review-pr\ndescription: Review a PR. Carefully.\n---\n\n1. Read the diff.\n")
        #expect(document.files == [SkillFile(path: "SKILL.md", data: Data(document.markdown.utf8), executable: false)])
    }

    @Test func aDescriptionYamlWouldMisreadIsQuoted() {
        #expect(SkillDocument.yamlScalar("Use it: always") == "\"Use it: always\"")
        #expect(SkillDocument.yamlScalar("- a list") == "\"- a list\"")
        #expect(SkillDocument.yamlScalar("Say \"hi\": twice") == "'Say \"hi\": twice'")
        #expect(SkillDocument.yamlScalar("Plain words") == "Plain words")
    }
}

/// The Skills screen against fake daemons, each with its own demo library
/// (`crates/herder-ffi/tests/support`), which the client brings onto one repository.
@MainActor
struct SkillsFleetTests {
    private static let demo = ["release-notes", "review-pr"]

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

    /// Whether every machine is on the library's repository at one commit, with `skills`.
    private func inSync(_ fleet: Fleet, skills: [String]) -> Bool {
        let library = SkillLibrary(fleet.machines)
        let heads = Set(library.checkouts.map(\.head))
        return library.checkouts.allSatisfy { $0.repo == library.repo && $0.head != nil && $0.pullError == nil }
            && heads.count == 1 && library.skills.map(\.name) == skills
            && library.skills.allSatisfy { $0.machines.count == library.checkouts.count }
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func addEditDeleteAndImportReachEveryMachine() async throws {
        let a = try FakeDaemon(name: "skills-a")
        let b = try FakeDaemon(name: "skills-b")
        let (fleet, following) = try await follow(pairing: [a, b])
        defer { following.cancel() }
        // Sync state: both machines end up on one repository at one commit, pulled.
        #expect(await eventually(within: 30) { inSync(fleet, skills: Self.demo) })
        let synced = SkillLibrary(fleet.machines)
        #expect(synced.checkouts.count == 2)
        #expect(synced.checkouts.allSatisfy { $0.lastPull != nil })
        let writer = try #require(synced.writer)

        // Add: written through one machine, pulled on the other.
        var document = SkillDocument(name: "triage", description: "Sort new issues by area.", body: "Label each issue.")
        try await fleet.putSkill(document, on: writer)
        #expect(await eventually(within: 30) { inSync(fleet, skills: ["release-notes", "review-pr", "triage"]) })
        #expect(SkillLibrary(fleet.machines).checkouts.first?.head != synced.checkouts.first?.head)

        // Edit: the new description reaches both.
        document.description = "Sort new issues by area and urgency."
        try await fleet.putSkill(document, on: writer)
        #expect(await eventually(within: 30) {
            let skill = SkillLibrary(fleet.machines).skills.first { $0.name == "triage" }
            return inSync(fleet, skills: ["release-notes", "review-pr", "triage"])
                && skill?.description == "Sort new issues by area and urgency."
        })

        // Delete.
        try await fleet.deleteSkill("triage", on: writer)
        #expect(await eventually(within: 30) { inSync(fleet, skills: Self.demo) })

        // Import from another repository, named after its folder.
        try await fleet.importSkill(gitURL: a.skillSource, path: "changelog", on: writer)
        #expect(await eventually(within: 30) { inSync(fleet, skills: ["changelog", "release-notes", "review-pr"]) })
        #expect(SkillLibrary(fleet.machines).skills.first?.description == "Keep CHANGELOG.md up to date.")

        // A pull answers on every machine.
        #expect(await fleet.pullSkills().isEmpty)
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aSkillIsEnabledPerMachine() async throws {
        let a = try FakeDaemon(name: "skills-on")
        let b = try FakeDaemon(name: "skills-off")
        let (fleet, following) = try await follow(pairing: [a, b])
        defer { following.cancel() }
        #expect(await eventually(within: 30) { inSync(fleet, skills: Self.demo) })

        try await fleet.setSkillEnabled("review-pr", false, on: "skills-off")
        #expect(await eventually {
            let review = SkillLibrary(fleet.machines).skills.first { $0.name == "review-pr" }
            return review?.machines.map(\.enabled) == [true, false]
        })
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aMemberSeesTheLibraryButCannotChangeIt() async throws {
        let daemon = try FakeDaemon(name: "skills-shared")
        let (member, following) = try await follow(pairing: [daemon], member: true)
        defer { following.cancel() }
        #expect(await eventually { SkillLibrary(member.machines).skills.map(\.name) == Self.demo })
        let library = SkillLibrary(member.machines)
        #expect(member.machines.first?.role == .member)
        #expect(library.writer == nil)
        #expect(library.setter == nil)
        #expect(library.skills.allSatisfy { $0.machines.allSatisfy { !$0.changeable } })
        #expect(library.pullable.isEmpty)
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aSessionListsTheProjectSkillsOfItsRepository() async throws {
        let daemon = try FakeDaemon(name: "skills-project")
        let (fleet, following) = try await follow(pairing: [daemon])
        defer { following.cancel() }
        _ = try await fleet.createSession(
            on: "skills-project", repo: daemon.repo, projectId: nil, accountId: daemon.account, model: "demo-model",
            mode: .ask, prompt: "Say hello.")
        #expect(await eventually(within: 20) { !ProjectSkills.all(fleet.machines).isEmpty })
        let skills = ProjectSkills.all(fleet.machines).first?.skills ?? []
        #expect(skills == [SessionSkill(name: "deploy", description: "Deploy the app to staging.", source: .project,
                                        path: ".claude/skills/deploy")])
    }
}
