import Herder
@testable import HerderKit
import Testing

struct CheckpointsTests {
    let blocks: [TranscriptBlock] = [
        .notice(Notice(id: 0, text: "Started", tone: .info)),
        .user(id: "u1", text: "Fix the build\nplease", outgoing: nil),
        .reasoning(id: "r1", text: "Thinking", streaming: false),
        .assistant(id: "a1", text: "**Fixed.**\nIt builds.", streaming: false),
        .assistant(id: "a2", text: "Anything else?", streaming: false),
        .user(id: "u2", text: "Ship it", outgoing: nil),
        .tools(id: "t1", calls: []),
    ]

    @Test func eachUserMessageIsACheckpointWithTheStartOfItsReply() {
        let checkpoints = Checkpoints(blocks)
        #expect(checkpoints.items.map(\.id) == ["u1", "u2"])
        #expect(checkpoints.items.map(\.prompt) == ["Fix the build please", "Ship it"])
        #expect(checkpoints.items.map(\.reply) == ["**Fixed.** It builds.", nil])
    }

    @Test func herdersFollowUpsAreNotCheckpoints() {
        let followUp = FollowUp(reason: .ciPassed, pr: 7, headSha: "abc123")
        let checkpoints = Checkpoints(blocks + [.user(id: "f1", text: "CI is green on #7.", outgoing: nil, followUp: followUp)])
        #expect(checkpoints.items.map(\.id) == ["u1", "u2"])
    }

    @Test func theBlockAtTheTopMarksTheCheckpointItFallsUnder() {
        let checkpoints = Checkpoints(blocks)
        #expect(checkpoints.current(top: "a2") == 0)
        #expect(checkpoints.current(top: "u2") == 1)
        #expect(checkpoints.current(top: "t1") == 1)
        #expect(checkpoints.current(top: "notice-0") == nil)
        #expect(checkpoints.current(top: nil) == nil)
    }
}
