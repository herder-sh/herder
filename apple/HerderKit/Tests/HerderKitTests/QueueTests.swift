import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct QueueTests {
    @Test func movesAsListsReportThem() {
        let ids = ["a", "b", "c"]
        #expect(QueueTray.move(2, to: 0, in: ids)! == ("c", "a"))
        #expect(QueueTray.move(0, to: 3, in: ids)! == ("a", nil))
        #expect(QueueTray.move(0, to: 2, in: ids)! == ("a", "c"))
        #expect(QueueTray.move(1, to: 1, in: ids) == nil)
        #expect(QueueTray.move(1, to: 2, in: ids) == nil)
        #expect(QueueTray.move(0, to: -1, in: ids) == nil)
    }

    @Test func mergesTheUsersOwnPromptsAroundOthers() {
        func prompt(_ id: String, by: String?, agent: Bool = false) -> QueuedPrompt {
            QueuedPrompt(
                promptId: id, text: id, images: 0, by: by,
                agentMessage: agent ? AgentMessage(senderSessionId: "s0", messageId: "m-\(id)", hopCount: 1, permissionCeiling: .ask) : nil)
        }
        let queue = [
            prompt("a", by: nil, agent: true), prompt("b", by: "alice"), prompt("c", by: "bob"),
            prompt("d", by: nil, agent: true), prompt("e", by: "alice"),
        ]
        #expect(QueueTray.mergeable(queue) == ["b", "e"])
        #expect(QueueTray.mergeable(Array(queue.prefix(3))) == [])
        #expect(QueueTray.mergeable([]) == [])
    }

    /// The fake daemon's hold account holds its first turn until it is interrupted; prompts
    /// queue behind it (`crates/herder-ffi/fixtures/hold.jsonl`).
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func theTrayRemovesReordersMergesAndSendsOneNowAndFollowsOtherClients() async throws {
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
            on: machine.hostId, repo: daemon.repo, projectId: nil, accountId: daemon.holdAccount, model: "",
            mode: .fullAccess, prompt: "Hold.")
        #expect(await eventually { fleet.sessions[key]?.turn != nil })

        for text in ["A.", "B.", "C."] { await fleet.submit(text, to: key) }
        let texts = { fleet.queue(of: key).map(\.text) }
        let id = { (text: String) in fleet.queue(of: key).first { $0.text == text }!.promptId }
        #expect(await eventually { texts() == ["A.", "B.", "C."] })
        // The tray has them now, not the transcript.
        #expect(await eventually { fleet.sessions[key]?.outbox.isEmpty == true })

        await fleet.removeQueued(id("B."), from: key)
        #expect(await eventually { texts() == ["A.", "C."] })
        await fleet.moveQueued(id("C."), before: id("A."), in: key)
        #expect(await eventually { texts() == ["C.", "A."] })
        #expect(fleet.refusals[key] == nil)

        // Another device of the same user moves it back; this tray follows.
        let shared = try await fleet.client.share()
        let other = try Client.open(configDir: temporaryProfile().path, client: "other")
        _ = try await other.pair(link: pairingLinkToString(link: shared.link))
        try await other.synced(hostId: machine.hostId)
        _ = try await other.send(
            hostId: machine.hostId,
            command: .moveQueued(sessionId: key.sessionId, promptId: id("A."), before: id("C.")))
        #expect(await eventually { texts() == ["A.", "C."] })

        // Two more merge into one, which keeps the first one's place.
        for text in ["D.", "E."] { await fleet.submit(text, to: key) }
        #expect(await eventually { texts() == ["A.", "C.", "D.", "E."] })
        await fleet.mergeQueued([id("D."), id("E.")], in: key)
        #expect(await eventually { texts() == ["A.", "C.", "D.\n\nE."] })
        #expect(fleet.refusals[key] == nil)

        // Sent now, it stops the held turn and runs first; the rest follows.
        let first = id("A.")
        await fleet.sendQueuedNow(first, in: key)
        #expect(fleet.refusals[key] == nil)
        #expect(await eventually {
            guard let model = fleet.sessions[key] else { return false }
            let users = Transcript.blocks(model).compactMap { block -> String? in
                if case .user(_, let text, _, _, _) = block { text } else { nil }
            }
            return users == ["Hold.", "A.", "C.", "D.\n\nE."] && model.turn == nil
        })
        #expect(texts().isEmpty)

        // Once it started, it can no longer be edited.
        await fleet.removeQueued(first, from: key)
        #expect(fleet.refusals[key] == "That message has already started.")
    }
}
