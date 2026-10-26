import Foundation
import Herder
@testable import HerderKit
import Testing

private func item(_ id: String, _ body: ItemBody, turn: String) -> EventBody {
    .itemAdded(item: Item(agentMessage: nil, parentCallId: nil, id: id, turnId: turn, body: body))
}

/// A turn `turn` asked `text`, that failed with `error`.
private func failedTurn(_ turn: String, _ text: String, _ error: TurnError) -> [EventBody] {
    [.turnStarted(turnId: turn), item("\(turn)-u", .userMessage(text: text, attachments: []), turn: turn),
     .turnFailed(turnId: turn, error: error)]
}

private let signedOut = TurnError(class: .auth, message: "Invalid API key · Please run /login")

private func account(_ id: String, used: Double = 10, needsLogin: Bool = false, fallback: Bool = false) -> Account {
    Account(accountId: id, provider: "claude", label: id == "main" ? "ops" : id, configDir: nil, email: nil,
            usage: [UsageWindow(window: "5h", usedPercent: used, resetsAt: nil)], fallback: fallback,
            needsLogin: needsLogin ? "2026-01-01T00:00:00Z" : nil)
}

private func context(_ status: SessionStatus, retryAt: Date? = nil, held: Int = 0, accounts: [Account] = [account("main")],
                     canSignIn: Bool = true) -> FailureCard.Context {
    FailureCard.Context(status: status, retryAt: retryAt, held: held, accounts: accounts, machine: "trash-can-01",
                        canSignIn: canSignIn)
}

struct FailureCardTests {
    @Test func anAuthFailureNamesTheAccountAndMachineAndOffersSignInAndAnotherAccount() throws {
        var script = Script()
        let model = script.model([created()] + failedTurn("t1", "provide some mockups", signedOut)
                                 + [.sessionStatusChanged(status: .needsLogin, retryAt: nil)])
        let failure = try #require(model.standingFailure)
        let card = FailureCard(failure, context(.needsLogin, held: 2,
                                                accounts: [account("main", needsLogin: true), account("work", used: 40)]))
        #expect(card.kind == .auth)
        #expect(card.title == "Claude signed out the ops account on trash-can-01")
        #expect(card.message == "Your 2 messages are held. They send as soon as the account works again.")
        #expect(card.actions == [.signIn, .continueOn("work", label: "work"), .details])
    }

    @Test func anAuthCardOffersNoOtherAccountWithoutHeadroom() throws {
        var script = Script()
        let model = script.model([created()] + failedTurn("t1", "go", signedOut))
        let failure = try #require(model.standingFailure)
        let accounts = [account("main"), account("full", used: 100), account("out", needsLogin: true)]
        let card = FailureCard(failure, context(.needsLogin, accounts: accounts))
        #expect(card.actions == [.signIn, .details])
        #expect(card.message == "Messages you send now wait until the account works again.")
    }

    @Test func aMemberCannotSignInSoTheCardSaysWhoCan() throws {
        var script = Script()
        let model = script.model([created()] + failedTurn("t1", "go", signedOut))
        let card = FailureCard(try #require(model.standingFailure), context(.needsLogin, canSignIn: false))
        #expect(card.actions == [.details])
        #expect(card.message.hasSuffix("Ask the machine’s owner to sign it in again."))
    }

    @Test func theOtherAccountIsTheRotatingOneWithTheMostRoom() {
        let accounts = [account("main"), account("busy", used: 70), account("roomy", used: 20),
                        account("spare", used: 0, fallback: true), FailureCardTests.codex]
        #expect(FailureCard.alternative(to: "main", provider: "claude", in: accounts)?.accountId == "roomy")
        #expect(FailureCard.alternative(to: "main", provider: "claude", in: [account("main"), account("spare", fallback: true)])?
            .accountId == "spare")
    }

    private static let codex = Account(accountId: "codex", provider: "codex", label: "codex", configDir: nil, email: nil, usage: [])

    @Test func aUsageLimitWaitingForItsResetOffersWaitAndAnotherAccount() throws {
        var script = Script()
        let limit = TurnError(class: .limitReached, message: "spend limit · your session limit resets 11:20pm")
        let at = Date(timeIntervalSince1970: 1_767_300_000)
        let model = script.model([created()] + failedTurn("t1", "go", limit)
                                 + [.sessionStatusChanged(status: .waitingForCapacity, retryAt: at.ISO8601Format())])
        #expect(model.retryAt == at)
        let failure = try #require(model.standingFailure)
        let card = FailureCard(failure, context(.waitingForCapacity, retryAt: model.retryAt,
                                                accounts: [account("main", used: 100), account("work")]))
        #expect(card.kind == .limit)
        #expect(card.title == "Claude usage limit reached on the ops account")
        #expect(card.message.hasPrefix("It resets at \(FailureCard.time(at))."))
        #expect(card.actions == [.wait(until: at), .continueOn("work", label: "work"), .details])
    }

    @Test func aUsageLimitWithNoRetryWaitingOffersRetryAndSaysTheResetItKnows() throws {
        var script = Script()
        let limit = TurnError(class: .limitReached, message: "spend limit · your session limit resets 11:20pm")
        let model = script.model([created()] + failedTurn("t1", "go", limit)
                                 + [.sessionStatusChanged(status: .needsYou, retryAt: nil)])
        let card = FailureCard(try #require(model.standingFailure), context(.needsYou))
        #expect(card.message == "It resets 11:20pm.")
        #expect(card.actions == [.retry, .details])
    }

    @Test func aTransientOrFatalFailureSaysItsFirstSentenceAndOffersRetry() throws {
        var script = Script()
        let error = TurnError(class: .transient, message: "Overloaded. The API is busy, try again · code 529")
        let model = script.model([created()] + failedTurn("t1", "go", error))
        let failure = try #require(model.standingFailure)
        #expect(failure.prompt == "go")
        let card = FailureCard(failure, context(.needsYou))
        #expect(card.kind == .problem)
        #expect(card.message == "Overloaded.")
        #expect(card.actions == [.retry, .details])
    }

    @Test func theSameFailureRepeatedIsOneCardWithItsCountAfterTheLastAttempt() {
        var script = Script()
        let model = script.model([created()] + failedTurn("t1", "provide some mockups", signedOut)
                                 + failedTurn("t2", "go", signedOut) + failedTurn("t3", "go on", signedOut))
        let failures = Transcript.blocks(model).compactMap { block -> Failure? in
            if case .failure(let failure) = block { failure } else { nil }
        }
        #expect(failures.count == 1)
        #expect(failures.first?.count == 3)
        #expect(failures.first?.prompt == "go on")
        #expect(failures.first?.line == "Turn failed: the account needs signing in again ×3")
        guard case .failure? = Transcript.blocks(model).last else {
            Issue.record("the failure is not last: \(Transcript.blocks(model))")
            return
        }
    }

    @Test func aDifferentFailureStandsInsteadAndTheFirstBecomesALine() {
        var script = Script()
        let model = script.model([created()] + failedTurn("t1", "go", signedOut)
                                 + failedTurn("t2", "go", TurnError(class: .fatal, message: "boom")))
        let failures = Transcript.blocks(model).compactMap { block -> Failure? in
            if case .failure(let failure) = block { failure } else { nil }
        }
        #expect(failures.map(\.resolution) == [.superseded, nil])
        #expect(model.standingFailure?.error.class == .fatal)
    }

    @Test func signingInAgainTurnsTheCardIntoAQuietLine() {
        var script = Script()
        let model = script.model([created()] + failedTurn("t1", "go", signedOut) + [
            .sessionStatusChanged(status: .needsLogin, retryAt: nil),
            .sessionStatusChanged(status: .running, retryAt: nil),
        ])
        #expect(model.standingFailure == nil)
        let failure = Transcript.blocks(model).compactMap { block -> Failure? in
            if case .failure(let failure) = block { failure } else { nil }
        }.first
        #expect(failure?.resolution == .signedIn)
        #expect(failure?.line == "Signed in again · resumed")
    }

    @Test func aNewTurnStartingClearsTheCardUntilItFailsTheSameWay() {
        let limit = TurnError(class: .limitReached, message: "You've hit your limit · resets 4:10am (Europe/Vilnius)")
        var script = Script()
        let retrying = script.model([created()] + failedTurn("t1", "go", limit) + [.turnStarted(turnId: "t2")])
        #expect(retrying.standingFailure == nil)
        let line = Transcript.blocks(retrying).compactMap { block -> Failure? in
            if case .failure(let failure) = block { failure } else { nil }
        }.first
        #expect(line?.resolution == .retried)
        #expect(line?.line.hasSuffix("· retried") == true)
        var again = Script()
        let failedAgain = again.model([created()] + failedTurn("t1", "go", limit) + failedTurn("t2", "go on", limit))
        #expect(failedAgain.standingFailure?.count == 2)
    }

    @Test func closingTheCardLeavesItsLine() throws {
        var script = Script()
        var model = script.model([created()] + failedTurn("t1", "go", TurnError(class: .fatal, message: "boom")))
        try #require(model.standingFailure != nil)
        model.dismissFailure()
        #expect(model.standingFailure == nil)
        let failure = Transcript.blocks(model).compactMap { block -> Failure? in
            if case .failure(let failure) = block { failure } else { nil }
        }.first
        #expect(failure?.resolution == .dismissed)
        #expect(failure?.line == "Turn failed: boom")
    }

    @Test func aCompletedTurnOrASwitchEndsTheFailure() {
        var script = Script()
        let completed = script.model([created()] + failedTurn("t1", "go", TurnError(class: .fatal, message: "boom"))
                                     + [.turnStarted(turnId: "t2"), .turnCompleted(turnId: "t2", usage: nil)])
        #expect(completed.standingFailure == nil)
        var other = Script()
        let switched = other.model([created()] + failedTurn("t1", "go", signedOut) + [.accountSwitched(accountId: "work")])
        #expect(switched.standingFailure == nil)
    }

    @Test func heldMessagesShowDimmedAboveTheCardWhileTheAccountNeedsSignIn() {
        var script = Script()
        let model = script.model([created()] + failedTurn("t1", "provide some mockups", signedOut)
                                 + [.sessionStatusChanged(status: .needsLogin, retryAt: nil)])
        let queue = [QueuedPrompt(promptId: "p1", text: "go", images: 0, by: nil, agentMessage: nil)]
        let blocks = Transcript.blocks(model, queue: queue)
        guard blocks.count >= 2, case .held("p1", "go") = blocks[blocks.count - 2], case .failure = blocks[blocks.count - 1] else {
            Issue.record("unexpected blocks: \(blocks)")
            return
        }
        // Once the account works, the queue runs from the tray as any other.
        var idle = model
        idle.status = .running
        #expect(Transcript.held(idle, queue: queue).isEmpty)
    }

    @Test func aSignedOutSessionSaysSoInTheHeaderAndOnTheBoard() {
        var script = Script()
        let model = script.model([created()] + failedTurn("t1", "go", signedOut)
                                 + [.sessionStatusChanged(status: .needsLogin, retryAt: nil)])
        #expect(model.stateLabel == "Needs sign-in")
        #expect(model.activity == "Claude account needs sign-in")
        #expect(TaskState(model.state) == .waitingForYou)
    }
}
