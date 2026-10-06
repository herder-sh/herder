import Foundation
import Herder
@testable import HerderKit
import SwiftTerm
import Testing

@MainActor
struct FleetTests {
    @Test func aProfileThatCannotBeOpenedSaysWhy() throws {
        let file = temporaryProfile()
        try Data().write(to: file)
        guard case .failed(let message) = Profile.open(at: file, client: "test") else {
            Issue.record("opened a profile at a file")
            return
        }
        #expect(!message.isEmpty)
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func pairingShowsTheMachineConnected() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        #expect(fleet.machines.isEmpty)
        let following = Task { await fleet.follow() }
        defer { following.cancel() }

        let machine = try await fleet.pair(daemon)
        #expect(machine.name == "fake-host")
        #expect(fleet.machines.map(\.hostId) == [machine.hostId])
        #expect(await eventually { fleet.machines.first?.connection == .connected })
        #expect(fleet.connectionLog[machine.hostId]?.last?.state == .connected)
        #expect(await eventually { fleet.machines.first?.role == .owner })
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func followingShowsAConnectionMadeBeforeIt() async throws {
        let daemon = try FakeDaemon()
        let profile = temporaryProfile()
        do {
            guard case .opened(let fleet) = Profile.open(at: profile, client: "test") else {
                Issue.record("cannot open a fresh profile")
                return
            }
            try await fleet.pair(daemon)
            fleet.suspend()
        }
        // Reopened, as on an app launch: the machine connects before the view follows.
        guard case .opened(let fleet) = Profile.open(at: profile, client: "test") else {
            Issue.record("cannot reopen the profile")
            return
        }
        let host = try #require(fleet.machines.first?.hostId)
        try await fleet.client.synced(hostId: host)
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        #expect(await eventually { fleet.machines.first?.connection == .connected })
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aSessionStartedFromTheAppShowsLiveRenamesAndArchives() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let machine = try await fleet.pair(daemon)
        try await fleet.client.synced(hostId: machine.hostId)

        let key = try await fleet.createSession(
            on: machine.hostId, repo: daemon.repo, projectId: nil, accountId: daemon.account, model: "",
            mode: .ask, prompt: "Say hello.")
        let sessionId = key.sessionId

        #expect(await eventually {
            fleet.lists.home.contains { $0.key.sessionId == sessionId && $0.activity == "Hello, world." }
        })
        #expect(fleet.lists.projects.flatMap(\.sessions).map(\.key.sessionId) == [sessionId])

        await fleet.rename(key, to: "Greeting")
        #expect(await eventually { fleet.lists.home.contains { $0.key == key && $0.title == "Greeting" } })
        #expect(fleet.toast == nil)

        await fleet.archive(key)
        #expect(await eventually { fleet.sessions[key]?.state == .archived })
        #expect(fleet.refusals[key] == nil)
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aFullTurnShowsInTheTranscript() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let machine = try await fleet.pair(daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        let key = try await fleet.createSession(
            on: machine.hostId, repo: daemon.repo, projectId: nil, accountId: daemon.account, model: "",
            mode: .fullAccess, prompt: "Say hello.")

        #expect(await eventually {
            guard let model = fleet.sessions[key] else { return false }
            return Transcript.blocks(model).contains { $0 == .assistant(id: $0.id, text: "Hello, world.", streaming: false) }
        })
        let blocks = Transcript.blocks(try #require(fleet.sessions[key]))
        #expect(blocks.contains { if case .user(_, "Say hello.", _, nil, nil, nil) = $0 { true } else { false } })
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aNewSessionShowsItsFirstPromptAtOnce() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let machine = try await fleet.pair(daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        let key = try await fleet.createSession(
            on: machine.hostId, repo: daemon.repo, projectId: nil, accountId: daemon.account, model: "",
            mode: .fullAccess, prompt: "Say hello.")

        // Before the machine journals it, the session view opens on the prompt, not on no turns.
        let blocks = Transcript.blocks(try #require(fleet.sessions[key]))
        #expect(blocks.contains { if case .user(_, "Say hello.", _, _, nil, nil) = $0 { true } else { false } })
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aShellOpensAndEchoes() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let machine = try await fleet.pair(daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        let key = try await fleet.createSession(
            on: machine.hostId, repo: daemon.repo, projectId: nil, accountId: daemon.account, model: "",
            mode: .fullAccess, prompt: "")

        let terminals = fleet.terminals(of: key)
        let shell = TerminalConnection(hostId: machine.hostId, terminalId: nil)
        terminals.connections.append(shell)
        let screen = shell.view(client: fleet.client, sessionId: key.sessionId)
        shell.connect(client: fleet.client, sessionId: key.sessionId, cols: 80, rows: 24)
        #expect(await eventually { shell.state == .attached })
        #expect(shell.terminalId != nil)
        shell.input(Array("echo herder-$((40+2))\n".utf8)[...])
        #expect(await eventually { Self.text(of: screen).contains("herder-42") })

        // Leaving the pane and coming back finds the same shell, still attached, same screen.
        #expect(fleet.terminals(of: key).connections.first === shell)
        #expect(shell.view(client: fleet.client, sessionId: key.sessionId) === screen)
        shell.input(Array("echo again-$((1+1))\n".utf8)[...])
        #expect(await eventually { Self.text(of: screen).contains("again-2") })
    }

    private static func text(of view: SwiftTerm.TerminalView) -> String {
        String(decoding: view.getTerminal().getBufferAsData(), as: UTF8.self)
    }

    /// A fake daemon paired and synced, with a session that has answered once.
    private func pairedSession() async throws -> (FakeDaemon, Fleet, Task<Void, Never>, SessionKey) {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            throw CocoaError(.fileReadUnknown)
        }
        let following = Task { await fleet.follow() }
        let machine = try await fleet.pair(daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        let key = try await fleet.createSession(
            on: machine.hostId, repo: daemon.repo, projectId: nil, accountId: daemon.account, model: "",
            mode: .fullAccess, prompt: "Say hello.")
        _ = await eventually { fleet.sessions[key]?.lastMessage == "Hello, world." }
        return (daemon, fleet, following, key)
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func sendingToAnArchivedSessionBringsItBack() async throws {
        let (daemon, fleet, following, key) = try await pairedSession()
        defer { following.cancel(); _ = daemon }
        await fleet.archive(key)
        #expect(await eventually { fleet.sessions[key]?.state == .archived })
        await fleet.unarchiveAndSubmit("Say hello.", images: [], to: key)
        #expect(fleet.refusals[key] == nil)
        #expect(await eventually { fleet.sessions[key]?.state != .archived })
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func archivingAWorktreeWithChangesKeepsThem() async throws {
        let (daemon, fleet, following, key) = try await pairedSession()
        defer { following.cancel(); _ = daemon }
        let worktree = try #require(await eventually { fleet.sessions[key]?.worktree != nil } ? fleet.sessions[key]?.worktree : nil)
        let notes = (worktree as NSString).appendingPathComponent("notes.txt")
        try "hi".write(toFile: notes, atomically: true, encoding: .utf8)

        await fleet.archive(key)
        #expect(fleet.archiveRefusal == nil)
        #expect(await eventually { fleet.sessions[key]?.state == .archived })
        #expect(fleet.toast?.undo == key)
        // The worktree goes days later, not now.
        #expect(FileManager.default.fileExists(atPath: notes))
    }

    /// Adding a project and its settings need a daemon with a config file, which the fake one
    /// lacks; browsing is all it can show.
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aMachinesFoldersAreBrowsedWithRepositoriesMarked() async throws {
        let (daemon, fleet, following, key) = try await pairedSession()
        defer { following.cancel() }
        let parent = (daemon.repo as NSString).deletingLastPathComponent
        let name = (daemon.repo as NSString).lastPathComponent
        let listing = try await fleet.listDirectory(parent, on: key.hostId)
        #expect(listing.entries.contains { $0.name == name && $0.isRepo })
    }
}
