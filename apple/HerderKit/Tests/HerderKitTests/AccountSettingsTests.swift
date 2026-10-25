import Herder
@testable import HerderKit
import Testing

struct AccountSettingsTests {
    @Test func accountDraftValidatesNamesAndHostPaths() {
        var draft = AccountDraft()
        #expect(draft.problem(existing: []) != nil)
        draft.id = " Claude-work.2 "
        #expect(draft.problem(existing: []) == nil)
        #expect(draft.problem(existing: ["Claude-work.2"]) != nil)
        draft.id = "work/account"
        #expect(draft.problem(existing: []) != nil)
        draft.id = "work"
        draft.configDir = "relative/path"
        #expect(draft.problem(existing: []) != nil)
        draft.configDir = "~/.claude-work"
        #expect(draft.problem(existing: []) == nil)
        #expect(!AccountDraft.validPath("/tmp/invalid\0path"))
        #expect(AccountDraft.validPath("/Users/me/login"))
        draft.label = " Personal "
        #expect(draft.account.label == "Personal")
        #expect(draft.account.configDir == "~/.claude-work")
        draft.configDir = " "
        #expect(draft.account.configDir == nil)
        #expect(AccountDraft.nextId(provider: "cursor", taken: []) == "cursor")
        #expect(AccountDraft.nextId(provider: "cursor", taken: ["cursor", "cursor-2"]) == "cursor-3")
    }

    @Test func suggestionsSkipAccountsAndLoginsStillRunning() {
        var box = machine("box", name: "box", sessions: [], accounts: [
            Account(accountId: "claude", provider: "claude", label: "claude", configDir: nil, email: nil, usage: []),
        ])
        box.terminals = [Terminal(terminalId: "t1", purpose: .login(accountId: "claude-2"))]
        let taken = AccountDraft.taken(on: box, pending: ["claude-3"])
        #expect(taken == ["claude", "claude-2", "claude-3"])
        var draft = AccountDraft(provider: "claude")
        draft.suggest(taken: taken)
        #expect(draft.id == "claude-4")
        #expect(draft.suggestedLabel == "Claude 4")
        #expect(draft.submitted.label == "Claude 4")
        draft.label = "Work"
        #expect(draft.submitted.label == "Work")
        // Switching provider moves the id along, until the user types one.
        draft.provider = "codex"
        draft.suggest(taken: taken)
        #expect(draft.id == "codex")
        #expect(draft.suggestedLabel == "Codex")
        draft.id = "codex-work"
        draft.idEdited = true
        draft.suggest(taken: ["codex-work"])
        #expect(draft.id == "codex-work")
        #expect(draft.problem(existing: ["codex-work"]) != nil)
    }

    @Test func aKnownEmailNamesTheSuggestion() {
        #expect(AccountDraft.nextId(provider: "claude", taken: ["claude"], email: "Sam.Lee@example.com") == "claude-sam.lee")
        #expect(AccountDraft.nextId(provider: "codex", taken: ["codex-sam"], email: "sam@example.com") == "codex-sam-2")
        #expect(AccountDraft.nextId(provider: "codex", taken: [], email: "@example.com") == "codex")
    }

    @MainActor
    @Test func loginsOnOneMachineEachKeepTheirOwnTerminal() {
        var terminals = ProviderTerminals()
        let claude = terminals.add(NewAccount(accountId: "claude-2", provider: "claude", label: nil, configDir: nil), on: "box")
        let codex = terminals.add(NewAccount(accountId: "codex", provider: "codex", label: nil, configDir: nil), on: "box")
        let elsewhere = terminals.add(NewAccount(accountId: "claude-2", provider: "claude", label: nil, configDir: nil), on: "laptop")
        let install = terminals.install("cursor", on: "box", from: nil)
        let relogin = terminals.relogin("claude", on: "box")
        #expect(Set([claude, codex, elsewhere, install, relogin]).count == 5)
        #expect(terminals.adds(on: "box") == [claude, codex])
        #expect(terminals.pendingIds(on: "box") == ["claude-2", "codex"])
        #expect(terminals.installs == [install])
        #expect(terminals[claude] !== terminals[codex])
        // The same login again while it runs is the one already running.
        let first = terminals[claude]
        terminals.add(NewAccount(accountId: "claude-2", provider: "claude", label: "Again", configDir: nil), on: "box")
        #expect(terminals[claude] === first)
        // Done forgets it, and only it.
        terminals.clear(claude)
        #expect(terminals[claude] == nil)
        #expect(terminals.adds(on: "box") == [codex])
        #expect(terminals.pendingIds(on: "box") == ["codex"])
        #expect(terminals[install] != nil && terminals[relogin] != nil)
    }

    @Test func theSheetListsRunningLoginsUntilAskedForAnother() {
        let one = ProviderJobKey(hostId: "box", job: .add("claude"))
        let two = ProviderJobKey(hostId: "box", job: .add("codex"))
        #expect(AddAccountPage.resolve(shown: nil, exists: false, adds: [], addingAnother: false) == .form)
        #expect(AddAccountPage.resolve(shown: nil, exists: false, adds: [one, two], addingAnother: false) == .logins)
        #expect(AddAccountPage.resolve(shown: nil, exists: false, adds: [one, two], addingAnother: true) == .form)
        #expect(AddAccountPage.resolve(shown: two, exists: true, adds: [one, two], addingAnother: true) == .job(two))
        // A login cleared while shown falls back to the others.
        #expect(AddAccountPage.resolve(shown: two, exists: false, adds: [one], addingAnother: false) == .logins)
    }

    @Test func progressReadsTheTerminalAndTheAccountList() {
        #expect(JobProgress(.connecting, added: false) == .starting)
        #expect(JobProgress(.attached, added: false) == .running)
        #expect(JobProgress(.attached, added: true) == .added)
        #expect(JobProgress(.exited(0), added: true) == .added)
        #expect(JobProgress(.exited(0), added: false) == .ended)
        #expect(JobProgress(.failed("no"), added: false) == .failed("no"))
        #expect(!JobProgress.running.finished && JobProgress.ended.finished)
    }

    @Test func anInstallReadsItsOutcomeFromTheVersionsBeforeAndAfter() {
        #expect(JobProgress(install: .connecting, before: "2.1.303", after: "2.1.303") == .starting)
        #expect(JobProgress(install: .attached, before: "2.1.303", after: "2.1.303") == .running)
        let updated = JobProgress(install: .exited(0), before: "2.1.303", after: "2.1.310")
        #expect(updated == .updated(from: "2.1.303", to: "2.1.310") && updated.label == "Updated 2.1.303 → 2.1.310")
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
    @Test func anInstallKeepsTheVersionItStartedFrom() {
        var terminals = ProviderTerminals()
        let key = terminals.install("claude", on: "box", from: "2.1.303")
        // Again while it runs: the same job, still from where it started.
        terminals.install("claude", on: "box", from: "2.1.310")
        #expect(terminals.before(key) == "2.1.303")
        terminals.clear(key)
        #expect(terminals.before(key) == nil)
    }

    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func everyInstallOpensItsTerminalWithoutBeingShown() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open profile")
            return
        }
        let machine = try await fleet.pair(daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        // No emulator is ever made: Update All shows one job's terminal at most. The fake
        // daemon installs no provider, and says so, which only an opened request learns.
        let keys = ["claude", "codex"].map { fleet.install($0, on: machine.hostId) }
        for key in keys {
            let connection = try #require(fleet.providerTerminals[key])
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
        let machine = try await fleet.pair(daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        let account = NewAccount(accountId: "work", provider: "claude", label: nil, configDir: nil)
        let key = fleet.providerTerminals.add(account, on: machine.hostId)
        let connection = try #require(fleet.providerTerminals[key])
        let screen = connection.view(client: fleet.client, sessionId: nil)
        connection.connect(client: fleet.client, sessionId: nil, cols: 80, rows: 24)
        #expect(await eventually {
            if case .failed = connection.state { return true }
            return false
        })
        #expect(fleet.providerTerminals[key] === connection)
        // Ended, it no longer holds its id, so the form may suggest it again.
        #expect(fleet.providerTerminals.pendingIds(on: machine.hostId).isEmpty)
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
        let machine = try await fleet.pair(daemon)
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
        let machine = try await fleet.pair(daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        let original = machine.accounts.first
        do {
            _ = try await fleet.client.send(hostId: machine.hostId, command: .setAccountSettings(
                accountId: original?.accountId ?? "fake", label: "Renamed", configDir: nil, fallback: nil))
            Issue.record("fake daemon must reject account edits without config persistence")
        } catch let error as HerderError {
            guard case .Rejected(let info) = error else { throw error }
            #expect(info.code == .badRequest)
        }
        #expect(machine.accounts.first?.label == original?.label)
    }

}
