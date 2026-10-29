import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct AnswerTests {
    /// The fake daemon's question account asks one question, and expects its answer to carry an
    /// image, which comes to the agent as the next prompt (`crates/herder-ffi/fixtures/question.jsonl`).
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func anAnswerWithAPhotoResolvesTheQuestionInOneStep() async throws {
        let daemon = try FakeDaemon()
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open a fresh profile")
            return
        }
        let following = Task { await fleet.follow() }
        defer { following.cancel() }
        let machine = try await fleet.signIn(to: daemon)
        try await fleet.client.synced(hostId: machine.hostId)
        let key = try await fleet.createSession(
            on: machine.hostId, repo: daemon.repo, projectId: nil, accountId: daemon.questionAccount, model: "",
            mode: .fullAccess, prompt: "Pick a layout.")
        #expect(await eventually { fleet.sessions[key]?.questions.count == 1 })

        let png = Data([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) + Data("png".utf8)
        await fleet.submit("Like this.", images: [Herder.Image(mediaType: "image/png", data: png)], to: key)
        #expect(fleet.refusals[key] == nil)
        // The script runs the image's prompt only after that answer; nothing waits in the queue.
        #expect(await eventually { fleet.sessions[key]?.questions.isEmpty == true })
        #expect(await eventually { fleet.sessions[key]?.status == .idle && fleet.sessions[key]?.turn == nil })
        #expect(fleet.queue(of: key).isEmpty)
        #expect(fleet.sessions[key]?.outbox.isEmpty == true)
    }
}
