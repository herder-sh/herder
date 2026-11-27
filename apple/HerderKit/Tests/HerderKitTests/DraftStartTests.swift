import Foundation
import Herder
@testable import HerderKit
import Testing

/// Sending from a draft does not hold the draft open: the fleet starts the session in the
/// background, listed as starting until its machine creates it.
@MainActor
struct DraftStartTests {
    let drafts = PromptDrafts(directory: FileManager.default.temporaryDirectory
        .appendingPathComponent("drafts-\(UUID().uuidString)", isDirectory: true))

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aSentDraftStartsInTheBackground() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let machine = try await fleet.signIn(to: daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        let draft = Draft(hostId: machine.hostId, repo: daemon.repo)

        let sent = fleet.start(draft, accountId: daemon.account, model: "", mode: .ask, prompt: "Say hello.",
                               drafts: drafts)
        // Listed at once, before the machine answers, so the draft can close.
        #expect(fleet.starting.map(\.prompt) == ["Say hello."])
        #expect(fleet.starting.first?.draft == draft)

        let key = try await sent.value.get()
        #expect(fleet.starting.isEmpty)
        #expect(await eventually {
            fleet.lists.projects.flatMap(\.sessions).contains { $0.key == key && $0.activity == "Hello, world." }
        })
        #expect(fleet.toast == nil)
        #expect(drafts.load(draft.key) == nil)
    }

    /// The machine lists the new session before its CLI is up to take the prompt: from then
    /// on its row, titled by the prompt, stands in for the starting card, never beside it.
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func theSessionsRowTakesTheStartingCardsPlace() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let machine = try await fleet.signIn(to: daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        let draft = Draft(hostId: machine.hostId, repo: daemon.repo)

        let sent = fleet.start(draft, accountId: daemon.account, model: "", mode: .ask, prompt: "Say hello.",
                               drafts: drafts)
        var both = false
        var listed: SessionSummary?
        let deadline = ContinuousClock.now + .seconds(120)
        while listed == nil, ContinuousClock.now < deadline {
            listed = fleet.lists.projects.flatMap(\.sessions).first
            both = both || (listed != nil && !fleet.starting.isEmpty)
            try await Task.sleep(for: .milliseconds(1))
        }
        let row = try #require(listed)
        #expect(!both)
        // Named from the first moment: by the prompt, or by the session once its events load.
        #expect(!row.title.hasPrefix("Session …"))
        _ = try await sent.value.get()
    }

    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func aStartThatFailsPutsThePromptBackAndSaysSo() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let machine = try await fleet.signIn(to: daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        let draft = Draft(hostId: machine.hostId, repo: daemon.repo)
        // Written in the draft while the first prompt was starting.
        drafts.save(.init(text: "And another thing.", images: [], draft: draft), for: draft.key)

        let sent = fleet.start(draft, accountId: "no-such-account", model: "", mode: .ask, prompt: "Say hello.",
                               drafts: drafts)
        #expect(fleet.starting.count == 1)
        guard case .failure = await sent.value else {
            Issue.record("started a session on an account the machine does not have")
            return
        }
        #expect(fleet.starting.isEmpty)
        #expect(drafts.load(draft.key)?.text == "Say hello.\n\nAnd another thing.")
        #expect(drafts.unsent.first?.draft == draft)
        let toast = try #require(fleet.toast)
        #expect(toast.failed)
        #expect(toast.text == "Could not start the session on \(machine.name)")
    }
}
