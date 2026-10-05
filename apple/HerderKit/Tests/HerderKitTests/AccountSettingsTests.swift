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
        let connection = TerminalConnection(hostId: machine.hostId, terminalId: nil, account: account)
        fleet.accountLogins[machine.hostId] = connection
        let screen = connection.view(client: fleet.client, sessionId: nil)
        connection.connect(client: fleet.client, sessionId: nil, cols: 80, rows: 24)
        #expect(await eventually {
            if case .failed = connection.state { return true }
            return false
        })
        #expect(fleet.accountLogins[machine.hostId] === connection)
        #expect(connection.view(client: fleet.client, sessionId: nil) === screen)
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
                accountId: original?.accountId ?? "fake", label: "Renamed", configDir: nil))
            Issue.record("fake daemon must reject account edits without config persistence")
        } catch let error as HerderError {
            guard case .Rejected(let info) = error else { throw error }
            #expect(info.code == .badRequest)
        }
        #expect(machine.accounts.first?.label == original?.label)
    }

}
