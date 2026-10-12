import Herder
import Testing
@testable import HerderKit

struct FollowUpEventTests {
    private func followUp(_ reason: FollowUpReason, pr: UInt64? = 345) -> FollowUp {
        FollowUp(reason: reason, pr: pr, headSha: pr.map { _ in "abc123" })
    }

    @Test func aFollowUpOnAPullRequestSaysWhatHappenedToIt() {
        #expect(followUp(.ciFailed).headline == "CI failed on #345")
        #expect(followUp(.ciPassed).headline == "CI passed on #345")
        #expect(followUp(.conflicting).headline == "#345 has merge conflicts")
        #expect(followUp(.changesRequested).headline == "Changes requested on #345")
        #expect(followUp(.stalled).headline == "Agent stopped with work unfinished")
        #expect(followUp(.unknown).headline == "herder followed up on #345")
    }

    @Test func aFollowUpWithoutAPullRequestStillSaysWhatHappened() {
        #expect(followUp(.ciFailed, pr: nil).headline == "CI failed")
        #expect(followUp(.ciPassed, pr: nil).headline == "CI passed")
        #expect(followUp(.conflicting, pr: nil).headline == "The pull request has merge conflicts")
        #expect(followUp(.changesRequested, pr: nil).headline == "Changes requested")
        #expect(followUp(.stalled, pr: nil).headline == "Agent stopped with work unfinished")
        #expect(followUp(.unknown, pr: nil).headline == "herder followed up")
    }

    @Test func aFollowUpSaysWhatHerderAskedTheAgentToDo() {
        for pr: UInt64? in [345, nil] {
            #expect(followUp(.ciFailed, pr: pr).request == "herder asked the agent to fix it and push")
            #expect(followUp(.ciPassed, pr: pr).request == "herder asked the agent to finish it")
            #expect(followUp(.conflicting, pr: pr).request == "herder asked the agent to rebase")
            #expect(followUp(.changesRequested, pr: pr).request == "herder asked the agent to address the review")
            #expect(followUp(.stalled, pr: pr).request == "herder asked the agent to carry on")
            #expect(followUp(.unknown, pr: pr).request == "herder sent the agent a prompt")
        }
    }

    @Test func aFollowUpSharesItsBoardColumnsSymbol() {
        #expect(followUp(.ciFailed).symbol == WorkState.ciFailed.symbol)
        #expect(followUp(.ciPassed).symbol == WorkState.readyToMerge.symbol)
        #expect(followUp(.conflicting).symbol == WorkState.conflicting.symbol)
        #expect(followUp(.changesRequested).symbol == WorkState.changesRequested.symbol)
    }
}
