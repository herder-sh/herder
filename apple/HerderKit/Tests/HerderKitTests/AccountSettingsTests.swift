import Foundation
import Herder
@testable import HerderKit
import Testing

struct AccountSettingsTests {
    @Test func aDraftValidatesItsConfigDirAndCleansItsFields() {
        var draft = AccountDraft()
        #expect(draft.problem == nil)
        draft.configDir = "relative/path"
        #expect(draft.problem != nil)
        draft.configDir = "~/.claude-work"
        #expect(draft.problem == nil)
        #expect(!AccountDraft.validPath("/tmp/invalid\0path"))
        #expect(AccountDraft.validPath("/Users/me/login"))
        draft.label = " Personal "
        #expect(draft.account(taken: []).label == "Personal")
        #expect(draft.account(taken: []).configDir == "~/.claude-work")
        draft.configDir = " "
        #expect(draft.account(taken: []).configDir == nil)
        #expect(AccountDraft.nextId(provider: "cursor", taken: []) == "cursor")
        #expect(AccountDraft.nextId(provider: "cursor", taken: ["cursor", "cursor-2"]) == "cursor-3")
    }

    @Test func eachMachineGetsTheFirstIdFreeThere() {
        var box = machine("box", name: "box", sessions: [], accounts: [
            Account(accountId: "claude", provider: "claude", label: "claude", configDir: nil, email: nil, usage: []),
        ])
        box.terminals = [Terminal(terminalId: "t1", purpose: .login(accountId: "claude-2"))]
        let taken = AccountDraft.taken(on: box, pending: ["claude-3"])
        #expect(taken == ["claude", "claude-2", "claude-3"])
        var draft = AccountDraft(provider: "claude")
        // One draft, two machines: each names the account the first way free on it.
        #expect(draft.account(taken: taken).accountId == "claude-4")
        #expect(draft.account(taken: taken).label == "Claude 4")
        #expect(draft.account(taken: []).accountId == "claude")
        #expect(draft.account(taken: []).label == "Claude")
        // A label names the id, and is the label on every machine.
        draft.label = "Work"
        #expect(draft.account(taken: taken).accountId == "claude-work")
        #expect(draft.account(taken: ["claude-work"]).accountId == "claude-work-2")
        #expect(draft.account(taken: ["claude-work"]).label == "Work")
        draft.provider = "codex"
        #expect(draft.account(taken: taken).accountId == "codex-work")
        // A login set up from another machine keeps its id wherever it is free.
        draft.id = "codex-sam"
        #expect(draft.account(taken: taken).accountId == "codex-sam")
        #expect(draft.account(taken: ["codex-sam"]).accountId == "codex-work")
    }

    @Test func anEndpointDraftNeedsAnHttpUrlAndAKey() {
        var draft = AccountDraft(provider: "claude", label: "Proxy")
        draft.kind = .endpoint
        #expect(draft.usesEndpoint)
        // Empty, it is not complete, though nothing is wrong yet.
        #expect(draft.problem == nil && !draft.complete)
        draft.url = "ftp://box.lan"
        #expect(draft.problem == "Use an http:// or https:// URL.")
        draft.url = "http://"
        #expect(draft.problem != nil)
        draft.url = " http://127.0.0.1:8317 "
        #expect(draft.problem == nil && !draft.complete)
        draft.key = "  "
        #expect(!draft.complete)
        draft.key = "sk-proxy"
        #expect(draft.complete)
        // A config dir typed for a login does not count: an endpoint account takes none.
        draft.configDir = "relative"
        #expect(draft.complete)
        #expect(AccountDraft.validEndpoint("https://proxy.example.com/v1"))
        #expect(!AccountDraft.validEndpoint("127.0.0.1:8317"))
    }

    @Test func onlyClaudeAndCodexRunAgainstAnEndpoint() {
        #expect(AccountDraft.supportsEndpoint("claude") && AccountDraft.supportsEndpoint("codex"))
        #expect(!AccountDraft.supportsEndpoint("cursor") && !AccountDraft.supportsEndpoint("opencode"))
        var draft = AccountDraft(provider: "cursor")
        draft.kind = .endpoint
        draft.configDir = "relative"
        // Cursor logs in, whatever the draft held for another provider.
        #expect(!draft.usesEndpoint)
        #expect(draft.problem != nil)
        #expect(draft.endpointCommand(taken: []) == nil)
    }

    @Test func anEndpointDraftAsksEachMachineForTheAccountUnderTheIdFreeThere() throws {
        var draft = AccountDraft(provider: "codex", label: "Proxy")
        draft.kind = .endpoint
        draft.url = " https://proxy.lan/v1 "
        #expect(draft.endpointCommand(taken: []) == nil)
        draft.key = " sk-1 "
        let command = try #require(draft.endpointCommand(taken: ["codex-proxy"]))
        guard case .addEndpointAccount(let id, let provider, let label, let endpoint, let key, _) = command else {
            Issue.record("expected addEndpointAccount, got \(command)")
            return
        }
        #expect(id == "codex-proxy-2" && provider == "codex" && label == "Proxy")
        #expect(endpoint == "https://proxy.lan/v1" && key == "sk-1")
        // Without a label, the provider names it, as a login's.
        draft.label = ""
        guard case .addEndpointAccount(let id, _, let label, _, _, _) = try #require(draft.endpointCommand(taken: [])) else {
            Issue.record("expected addEndpointAccount")
            return
        }
        #expect(id == "codex" && label == "Codex")
    }

    @MainActor @Test func theVaultsLoginsAreOfferedOnlyWhereAMachineReachesThem() {
        #expect(AddAccountForm.kinds(vault: false).map(\.value) == [.login, .endpoint])
        let kinds = AddAccountForm.kinds(vault: true)
        #expect(kinds.map(\.value) == [.login, .endpoint, .vault])
        #expect(kinds.last?.label == "Use the vault’s logins")
        let machines = [machine("a", name: "work-mbp", sessions: []), machine("b", name: "studio", sessions: [])]
        let vaults: [HostId: String] = ["a": "https://cpa.lan:8317"]
        // Logging in, either machine takes it; on the vault's logins, only the one that has them.
        #expect(AccountTarget.all(machines, provider: "claude", vaults: vaults).allSatisfy { $0.problem == nil })
        let targets = AccountTarget.all(machines, provider: "claude", vaults: vaults, usesVault: true)
        #expect(targets.first { $0.hostId == "a" }?.problem == nil)
        #expect(targets.first { $0.hostId == "a" }?.vault == "https://cpa.lan:8317")
        #expect(targets.first { $0.hostId == "b" }?.problem == "No vault logins")
    }

    @MainActor @Test func aVaultLoginsDraftNamesTheVaultsKeyOnEachMachinesOwnProxy() throws {
        var draft = AccountDraft(provider: "claude")
        draft.kind = .vault
        // Nothing to type: no URL, no key.
        #expect(draft.usesVault && draft.sendsEndpoint && !draft.usesEndpoint)
        #expect(draft.complete && draft.problem == nil)
        #expect(AddAccountSheet.hint(draft) == "Each machine uses its own key from the vault.")
        // A machine without the vault's logins gets nothing.
        #expect(draft.endpointCommand(taken: []) == nil)
        let command = try #require(draft.endpointCommand(taken: ["claude"], vault: "https://cpa.lan:8317"))
        guard case .addEndpointAccount(let id, let provider, let label, let endpoint, let key, let keySecret) = command else {
            Issue.record("expected addEndpointAccount, got \(command)")
            return
        }
        #expect(id == "claude-vault" && provider == "claude" && label == "Claude vault")
        #expect(endpoint == "https://cpa.lan:8317" && key == nil && keySecret == "CLIPROXYAPI_KEY")
        // A label typed names it, as any account's.
        draft.label = "Fleet"
        guard case .addEndpointAccount(let id, _, let label, _, _, _)
            = try #require(draft.endpointCommand(taken: [], vault: "https://cpa.lan:8317")) else {
            Issue.record("expected addEndpointAccount")
            return
        }
        #expect(id == "claude-fleet" && label == "Fleet")
        // Cursor has no endpoint: it logs in.
        draft.provider = "cursor"
        #expect(!draft.usesVault && !draft.sendsEndpoint)
    }

    @Test func aMemberSeesNoKeyField() {
        let machines = [machine("a", name: "team-box", sessions: [], role: .member)]
        let targets = AccountTarget.all(machines, provider: "claude")
        #expect(targets.map(\.owner) == [false])
        #expect(AccountTarget.all([machine("b", name: "mine", sessions: [])], provider: "claude").map(\.owner) == [true])
        #expect(AddAccountSheet.title(2, endpoint: true) == "Add on 2 machines")
        #expect(AddAccountSheet.title(1) == "Log in on 1 machine")
    }

    @Test func aKnownEmailNamesTheSuggestion() {
        #expect(AccountDraft.nextId(provider: "claude", taken: ["claude"], name: "Sam.Lee@example.com") == "claude-sam.lee")
        #expect(AccountDraft.nextId(provider: "codex", taken: ["codex-sam"], name: "sam@example.com") == "codex-sam-2")
        #expect(AccountDraft.nextId(provider: "codex", taken: [], name: "@example.com") == "codex")
        #expect(AccountDraft.nextId(provider: "codex", taken: [], name: "Codex") == "codex")
    }

    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func addingAnAccountOnTwoMachinesRunsTwoLoginsAndAddsItOnBoth() async throws {
        let one = try FakeDaemon(name: "one")
        let two = try FakeDaemon(name: "two")
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let first = try await fleet.signIn(to: one)
        let second = try await fleet.signIn(to: two)
        try await fleet.client.synced(hostId: first.hostId)
        try await fleet.client.synced(hostId: second.hostId)
        // The fake daemons log OpenCode in at once, in the config dir given.
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: dir) }
        var draft = AccountDraft(provider: "opencode", label: "Work")
        draft.configDir = dir.path
        let keys = fleet.addAccount(draft, on: [first.hostId, second.hostId])
        #expect(keys == [JobKey(hostId: first.hostId, kind: .add("opencode-work")),
                         JobKey(hostId: second.hostId, kind: .add("opencode-work"))])
        #expect(keys.allSatisfy { fleet.activity.phase($0) != .ended })
        let added = { (hostId: HostId) in
            fleet.machines.first { $0.hostId == hostId }?.accounts.first { $0.accountId == "opencode-work" }
        }
        #expect(await eventually { added(first.hostId) != nil && added(second.hostId) != nil })
        #expect(added(first.hostId)?.label == "Work" && added(second.hostId)?.label == "Work")
        #expect(await eventually { keys.allSatisfy { fleet.progress($0) == .added } })
    }

    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func anEndpointAccountIsAddedOnEachMachineAndEditedThere() async throws {
        let one = try FakeDaemon(name: "one")
        let two = try FakeDaemon(name: "two")
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let first = try await fleet.signIn(to: one)
        let second = try await fleet.signIn(to: two)
        try await fleet.client.synced(hostId: first.hostId)
        try await fleet.client.synced(hostId: second.hostId)
        var draft = AccountDraft(provider: "claude", label: "Proxy")
        draft.kind = .endpoint
        draft.url = "http://127.0.0.1:8317"
        draft.key = "sk-proxy"
        // No terminal, no job in Activity: the command goes straight to each machine.
        let failures = await fleet.addEndpointAccount(draft, on: [first.hostId, second.hostId])
        #expect(failures.isEmpty)
        #expect(fleet.activity.keys.isEmpty)
        let added = { (hostId: HostId) in
            fleet.machines.first { $0.hostId == hostId }?.accounts.first { $0.endpoint != nil }
        }
        #expect(await eventually { added(first.hostId) != nil && added(second.hostId) != nil })
        let account = try #require(added(first.hostId))
        #expect(account.label == "Proxy" && account.endpoint == "http://127.0.0.1:8317")
        // One login across both machines, by its endpoint.
        let login = try #require(ProviderAccounts(machines: fleet.machines).groups.flatMap(\.logins)
            .first { $0.endpoint != nil })
        #expect(login.on.count == 2)

        // An owner moves it to another endpoint and replaces its key.
        _ = try await fleet.client.send(hostId: first.hostId, command: account.settings(
            label: account.label, configDir: account.configDir, fallback: nil,
            endpoint: "https://vault.lan:8317", key: "sk-new"))
        #expect(await eventually { added(first.hostId)?.endpoint == "https://vault.lan:8317" })

        // A machine that refuses the account says why, for itself.
        var codex = draft
        codex.provider = "codex"
        let refused = await fleet.addEndpointAccount(codex, on: [first.hostId])
        if let reason = refused[first.hostId] { #expect(!reason.contains("sk-proxy")) }
    }

    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aVaultLoginsAccountIsAddedOnEachMachineWithTheVaultsKey() async throws {
        let one = try FakeDaemon(name: "one")
        let two = try FakeDaemon(name: "two")
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let first = try await fleet.signIn(to: one)
        let second = try await fleet.signIn(to: two)
        try await fleet.client.synced(hostId: first.hostId)
        try await fleet.client.synced(hostId: second.hostId)
        // A fake daemon links to no vault with a CLIProxyAPI: each says where its vault's is.
        for hostId in [first.hostId, second.hostId] {
            fleet.backupLinks[hostId] = .linked(LinkedVault(address: "vault.lan:7447", fingerprint: "ab", backup: true,
                                                            secrets: true, cliproxyapi: "https://cpa.lan:8317"))
        }
        var draft = AccountDraft(provider: "claude")
        draft.kind = .vault
        let failures = await fleet.addEndpointAccount(draft, on: [first.hostId, second.hostId])
        #expect(failures.isEmpty)
        #expect(fleet.activity.keys.isEmpty)
        let added = { (hostId: HostId) in
            fleet.machines.first { $0.hostId == hostId }?.accounts.first { $0.endpoint != nil }
        }
        #expect(await eventually { added(first.hostId) != nil && added(second.hostId) != nil })
        for hostId in [first.hostId, second.hostId] {
            let account = try #require(added(hostId))
            #expect(account.endpoint == "https://cpa.lan:8317" && account.keySecret == "CLIPROXYAPI_KEY")
            #expect(account.label == "Claude vault")
        }
        // One login across both machines, by its endpoint, its key from the vault.
        let login = try #require(ProviderAccounts(machines: fleet.machines).groups.flatMap(\.logins)
            .first { $0.endpoint != nil })
        #expect(login.on.count == 2 && login.keyFromVault)
    }

    @Test func progressReadsTheTerminalAndTheAccountList() {
        #expect(JobProgress(.connecting, added: false) == .starting)
        #expect(JobProgress(.attached, added: false) == .running)
        #expect(JobProgress(.attached, added: true) == .added)
        #expect(JobProgress(.exited(0), added: true) == .added)
        #expect(JobProgress(.exited(0), added: false) == .ended && JobProgress.ended.label == "Stopped")
        #expect(JobProgress(.failed("no"), added: false) == .failed("no"))
        #expect(!JobProgress.running.finished && JobProgress.ended.finished)
    }

    @Test func anInstallReadsItsOutcomeFromTheVersionsBeforeAndAfter() {
        #expect(JobProgress(install: .connecting, before: "2.1.303", after: "2.1.303") == .starting)
        // Running, it needs no one: not the amber "Waiting for you" of a login.
        let installing = JobProgress(install: .attached, before: "2.1.303", after: "2.1.303")
        #expect(installing == .installing && installing.label == "Installing" && !installing.finished)
        let updated = JobProgress(install: .exited(0), before: "2.1.303", after: "2.1.310")
        #expect(updated == .updated(from: "2.1.303", to: "2.1.310") && updated.label == "2.1.303 → 2.1.310")
        let installed = JobProgress(install: .exited(0), before: nil, after: "0.9.4")
        #expect(installed == .installed("0.9.4") && installed.label == "Installed 0.9.4")
        #expect(JobProgress(install: .exited(0), before: "2.1.310", after: "2.1.310") == .upToDate("2.1.310"))
        // A non-zero exit fails, whatever the versions say; so does a CLI still missing.
        #expect(JobProgress(install: .exited(1), before: "2.1.303", after: "2.1.310").label == "Failed")
        #expect(JobProgress(install: .exited(nil), before: nil, after: nil).label == "Failed")
        #expect(JobProgress(install: .exited(0), before: nil, after: nil).label == "Failed")
        #expect(JobProgress(install: .failed("no"), before: nil, after: nil) == .failed("no"))
        #expect(updated.finished && installed.finished)
    }

    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func everyInstallOpensItsTerminalWithoutBeingShown() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open profile")
            return
        }
        let machine = try await fleet.signIn(to: daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        // No emulator is ever made: a job runs whether its terminal is shown or not. The fake
        // daemon installs no provider, and says so, which only an opened request learns.
        let keys = ["claude", "codex"].map { fleet.install($0, on: machine.hostId) }
        for key in keys {
            let connection = try #require(fleet.activity[key]?.connection)
            #expect(await eventually {
                if case .failed = connection.state { return true }
                return false
            })
        }
    }

    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func unsupportedLoginShowsAnErrorAndKeepsTheTerminal() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open profile")
            return
        }
        let machine = try await fleet.signIn(to: daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        let account = NewAccount(accountId: "work", provider: "claude", label: nil, configDir: nil)
        let key = fleet.addAccount(account, on: machine.hostId)
        let connection = try #require(fleet.activity[key]?.connection)
        let screen = connection.view(client: fleet.client, sessionId: nil)
        #expect(await eventually {
            if case .failed = connection.state { return true }
            return false
        })
        #expect(fleet.activity[key]?.connection === connection)
        // Ended, it no longer holds its id, so the form may suggest it again.
        #expect(await eventually { fleet.activity.phase(key) == .ended })
        #expect(fleet.activity.pendingIds(on: machine.hostId).isEmpty)
        #expect(fleet.progress(key).failed)
        #expect(connection.view(client: fleet.client, sessionId: nil) === screen)
        // A login the machine never opened has no terminal there to cancel.
        await #expect(throws: HerderError.self) { try await fleet.cancelLogin(key) }
    }
    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func loggingInAgainAsksTheDaemonForTheAccountsLogin() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open profile")
            return
        }
        let machine = try await fleet.signIn(to: daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        // The fake daemon cannot log its fake provider in, and says so.
        let listed = { fleet.machines.first { $0.hostId == machine.hostId }?.accounts.first?.accountId }
        #expect(await eventually { listed() != nil })
        let accountId = try #require(listed())
        let connection = TerminalConnection(hostId: machine.hostId, terminalId: nil, relogin: accountId)
        connection.connect(client: fleet.client, sessionId: nil, cols: 80, rows: 24)
        #expect(await eventually {
            if case .failed(let message) = connection.state { return message.contains("cannot log") }
            return false
        })
    }

    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func editingReceivesDaemonErrorsWithoutChangingTheAccount() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open profile")
            return
        }
        let machine = try await fleet.signIn(to: daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        let original = machine.accounts.first
        do {
            _ = try await fleet.client.send(hostId: machine.hostId, command: .setAccountSettings(
                accountId: original?.accountId ?? "fake", label: "Renamed", configDir: nil, fallback: nil,
                endpoint: nil, key: nil))
            Issue.record("fake daemon must reject account edits without config persistence")
        } catch let error as HerderError {
            guard case .Rejected(let info) = error else { throw error }
            #expect(info.code == .badRequest)
        }
        #expect(machine.accounts.first?.label == original?.label)
    }

}
