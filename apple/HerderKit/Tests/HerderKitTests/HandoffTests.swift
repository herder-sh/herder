import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct HandoffTests {
    let source = SessionKey(hostId: "source", sessionId: "original")

    func account(_ id: String, _ provider: Provider = "claude") -> Account {
        Account(accountId: id, provider: provider, label: id.capitalized, configDir: nil, usage: [])
    }

    /// The source, a machine with one Claude account, one with two, and one with none.
    func machines() -> [Machine] {
        var here = machine("source", name: "Laptop", sessions: ["original"])
        here.accounts = [account("main")]
        var one = machine("one", name: "Studio", sessions: [])
        one.accounts = [account("studio"), account("gpt", "codex")]
        var two = machine("two", name: "trash-can-01", sessions: [])
        two.accounts = [account("personal"), account("work")]
        var none = machine("none", name: "Server", sessions: [])
        none.accounts = [account("gpt", "codex")]
        return [here, one, two, none]
    }

    func fleet(_ machines: [Machine]) throws -> Fleet {
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CocoaError(.fileNoSuchFile)
        }
        fleet.setMachinesForTesting(machines)
        return fleet
    }

    @Test func machinesSayWhyTheyCannotTakeTheSession() {
        var host = machines()[1]
        #expect(Fleet.handoffBlocker(host, provider: "claude") == nil)
        #expect(Fleet.handoffBlocker(host, provider: "gemini") == "No Gemini account")
        host.connection = .disconnected(error: "gone")
        #expect(Fleet.handoffBlocker(host, provider: "claude") == "Offline")
        host.connection = .connected
        host.role = .member
        #expect(Fleet.handoffBlocker(host, provider: "claude") == "Owner only")
        host.role = .owner
        host.hosts = [FleetHost(hostId: "replicated", hostName: "replicated", online: false, lastSeen: "")]
        #expect(Fleet.handoffBlocker(host, provider: "claude") == "Vault")
    }

    @Test func pickingAMachineHandsTheSessionOffAndAnAccountPicksWithinIt() throws {
        let fleet = try fleet(machines())
        var picked: [String] = []
        let section = fleet.machineSection(for: source, provider: "claude", forkable: true) { host, account in
            picked.append("\(host)/\(account ?? "default")")
        }
        #expect(section.options.map(\.title) == ["Laptop", "Studio", "trash-can-01", "Server"])
        #expect(section.options.map(\.unavailable) == [nil, nil, nil, "No Claude account"])
        // One Claude account: picking the machine hands off to it on its default.
        #expect(section.options[1].children.isEmpty)
        section.perform(.option(.machine, "one"))
        // Several: the machine opens its accounts, and an account hands off on it.
        #expect(section.options[2].children.map(\.title) == ["Personal", "Work"])
        #expect(section.opens(.option(.machine, "two")))
        section.perform(.option(.machine, "two"))
        section.perform(.child(.machine, "two", "work"))
        // The current machine changes nothing; "Fork Session" forks onto it.
        section.perform(.option(.machine, "source"))
        section.perform(.action(.machine))
        #expect(picked == ["one/default", "two/work", "source/default"])
        #expect(section.entries(expanded: nil).count == 4)
        #expect(section.entries(expanded: "two").contains(.child(.machine, "two", "personal")))
        // A child session cannot be forked.
        let child = fleet.machineSection(for: source, provider: "claude", forkable: false) { _, _ in }
        #expect(child.options.dropFirst().allSatisfy { $0.unavailable == "Cannot fork" })
        #expect(child.action?.enabled == false)
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func handingOffForksAndOpensTheForkAndAFailureShowsAToast() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let host = try await fleet.pair(daemon)
        let original = try await fleet.createSession(
            on: host.hostId, repo: daemon.repo, projectId: nil, accountId: daemon.account, model: "",
            mode: .ask, prompt: "Say hello.")
        #expect(await eventually { fleet.sessions[original]?.lastMessage != nil && fleet.sessions[original]?.status == .idle })
        let provider = try #require(fleet.sessions[original]?.provider)

        // "Fork Session" in the machine menu forks onto the same machine and opens the fork.
        var opened: [SessionKey] = []
        let section = fleet.machineSection(for: original, provider: provider, forkable: true) { target, account in
            Task {
                if let fork = await fleet.handOff(original, to: target, account: account) { opened.append(fork) }
            }
        }
        section.perform(.action(.machine))
        #expect(await eventually { !opened.isEmpty })
        let fork = try #require(opened.first)
        #expect(fork.hostId == host.hostId && fork.sessionId != original.sessionId)
        #expect(fleet.forkOrigins[fork] == ForkOrigin(sessionId: original.sessionId, hostId: host.hostId))
        #expect(fleet.handoffs.isEmpty)
        #expect(fleet.toast?.failed == false)
        #expect(await eventually {
            fleet.sessions[fork].map { $0.loaded && $0.lastMessage == "Hello, world." } == true
        })

        // A session the machine does not have cannot be handed off: a toast says why.
        let missing = SessionKey(hostId: host.hostId, sessionId: "01MISSING")
        #expect(await fleet.handOff(missing, to: host.hostId, account: nil) == nil)
        let toast = try #require(fleet.toast)
        #expect(toast.failed)
        #expect(toast.text.hasPrefix("Could not fork the session: "))
        #expect(fleet.handoffs.isEmpty)
    }
}
