import Foundation
import Herder
@testable import HerderKit
import Testing

private let sender = AgentMessage(senderSessionId: "source-session", messageId: "review-1", hopCount: 1, permissionCeiling: .ask)

private func agentPrompt(_ id: String, text: String, origin: AgentMessage?) -> EventBody {
    .itemAdded(item: Item(agentMessage: origin, parentCallId: nil, id: id, turnId: "t1",
                          body: .userMessage(text: text, attachments: [])))
}

struct AgentMessageTests {
    @Test func senderMetadataSurvivesTranscriptReplay() {
        var script = Script()
        let events = [created(), agentPrompt("incoming", text: "Review the account change", origin: sender)]
        let live = script.model(events)
        var replayScript = Script()
        let replay = replayScript.model(events)
        let expected = TranscriptBlock.user(id: "t1/incoming", text: "Review the account change", outgoing: nil, agentMessage: sender)
        #expect(Transcript.blocks(live).contains(expected))
        #expect(Transcript.blocks(replay).contains(expected))
        #expect(replay.timeline.last?.text == "From agent source-session: Review the account change")
    }

    @Test func anAgentPromptCannotAcknowledgeAnIdenticalHumanOutboxEntry() {
        var script = Script()
        var model = script.model([created()])
        let outgoing = Outgoing(text: "Run the tests", state: .delivered)
        model.outbox = [outgoing]
        model.apply(script.event(agentPrompt("agent", text: outgoing.text, origin: sender)))
        #expect(model.outbox == [outgoing])
        #expect(Transcript.blocks(model).contains(.user(id: "t1/agent", text: outgoing.text, outgoing: nil, agentMessage: sender)))
        model.apply(script.event(agentPrompt("human", text: outgoing.text, origin: nil)))
        #expect(model.outbox.isEmpty)
    }

    @Test func aFollowUpIsFromHerderNotTheUser() {
        var script = Script()
        let followUp = FollowUp(reason: .ciFailed, pr: 7, headSha: "abc123")
        let text = "CI failed on #7: test. Find out why, fix it and push."
        let event = EventBody.itemAdded(item: Item(agentMessage: nil, followUp: followUp, parentCallId: nil, id: "nudge",
                                                   turnId: "t1", body: .userMessage(text: text, attachments: [])))
        var model = script.model([created()])
        let outgoing = Outgoing(text: text, state: .delivered)
        model.outbox = [outgoing]
        model.apply(script.event(event))
        // Not the user's own prompt arriving.
        #expect(model.outbox == [outgoing])
        #expect(Transcript.blocks(model).contains(.user(id: "t1/nudge", text: text, outgoing: nil, followUp: followUp)))
    }

    @Test func attributionIsNotInferredFromText() {
        var script = Script()
        let text = "Sent by another agent: please review"
        let model = script.model([created(), agentPrompt("human", text: text, origin: nil)])
        #expect(Transcript.blocks(model).contains(.user(id: "t1/human", text: text, outgoing: nil)))
        #expect(model.timeline.last?.text == "Prompt: \(text)")
    }
}
