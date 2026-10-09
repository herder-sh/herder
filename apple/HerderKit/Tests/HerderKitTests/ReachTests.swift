import Herder
@testable import HerderKit
import Testing

struct ReachTests {
    private func project(_ id: ProjectId, remote: String?) -> Project {
        Project(projectId: id, name: "app", paths: ["/src/app"], remote: remote, defaultPermissionMode: nil,
                defaultAccount: nil, setupCommand: nil)
    }

    private func account(_ id: AccountId, _ provider: String, email: String? = nil, configDir: String? = nil) -> Account {
        Account(accountId: id, provider: provider, label: id.capitalized, configDir: configDir, email: email, usage: [])
    }

    @Test func aProjectWithARemoteIsMissingOnTheOtherMachines() {
        let app = project("github.com/org/app", remote: "git@github.com:org/app.git")
        let vault = FleetHost(hostId: "a", hostName: "laptop", online: true, lastSeen: "2026-01-01T00:00:00Z")
        let machines = [
            machine("a", name: "laptop", sessions: [], projects: [app]),
            machine("b", name: "server", sessions: []),
            machine("c", name: "mini", sessions: [], role: .member, connection: .connecting),
            machine("d", name: "vault", sessions: [], hosts: [vault]),
            machine("e", name: "never connected", sessions: [], role: nil),
        ]
        let reach = ProjectReach(projectId: "github.com/org/app", machines: machines)
        #expect(reach.remote == "git@github.com:org/app.git")
        #expect(reach.missing == [
            .init(hostId: "b", name: "server", connected: true, owner: true),
            .init(hostId: "c", name: "mini", connected: false, owner: false),
        ])

        // A project without a remote stays where it is.
        let local = project("a:/src/scratch", remote: nil)
        let scratch = ProjectReach(projectId: "a:/src/scratch",
                                   machines: [machine("a", name: "laptop", sessions: [], projects: [local]),
                                              machine("b", name: "server", sessions: [])])
        #expect(scratch.remote == nil)
        #expect(scratch.missing.isEmpty)
    }

    @Test func loginsAreMatchedByEmailElseIdAndMissingOnesAreSetUpAlike() {
        let cursor = ProviderStatus(provider: "cursor", installed: false, version: nil, binary: nil, canInstall: true,
                                    canUpdate: false)
        let machines = [
            machine("a", name: "laptop", sessions: [],
                    accounts: [account("claude-work", "claude", email: "me@work.test", configDir: "~/.claude-work"),
                               account("cursor", "cursor", configDir: "/opt/cursor")]),
            // The same login under another id.
            machine("b", name: "server", sessions: [],
                    accounts: [account("work", "claude", email: "me@work.test"), account("claude-work", "codex")],
                    providers: [cursor]),
            machine("c", name: "mini", sessions: [], role: .member),
        ]
        let groups = ProviderAccounts(machines: machines).groups
        #expect(groups.map(\.provider) == ["claude", "codex", "cursor"])
        let work = groups[0].logins[0]
        #expect(work.email == "me@work.test")
        #expect(work.on.map(\.name) == ["laptop", "server"])
        #expect(work.missing.map(\.name) == ["mini"])
        #expect(work.missing[0].owner == false)
        let draft = work.draft(taken: [])
        #expect(draft.account == NewAccount(accountId: "claude-work", provider: "claude", label: "Claude-Work",
                                            configDir: "~/.claude-work"))
        // An id taken there gets the provider's next free one.
        #expect(work.draft(taken: ["claude-work"]).id == "claude")

        let missingCursor = groups[2].logins[0].missing
        #expect(missingCursor.map(\.name) == ["server", "mini"])
        #expect(missingCursor[0].needsInstall)
        #expect(groups[2].logins[0].draft(taken: []).configDir == "")
    }
}
