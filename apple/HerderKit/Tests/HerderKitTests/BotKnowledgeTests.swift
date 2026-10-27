import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct BotKnowledgeTests {
    static let thread = ThreadLink(kind: .slack, botId: "trash-can", channelId: "C1", threadId: "1.2",
                                   url: "https://acme.slack.com/archives/C1/p12")

    /// A machine with the bot `trash-can`, which keeps its memory as the daemon does.
    @MainActor final class Machine {
        var bot = BotsTests.bot("trash-can")
        var facts: [BotFact] = []
        var sent: [CommandBody] = []
        var refuse: String?

        func knowledge(editable: Bool = true) async -> BotKnowledge {
            let bots = MachineBots(hostId: "devbox") { [self] command in
                sent.append(command)
                if let refuse { throw HerderError.Rejected(info: ErrorInfo(code: .badRequest, message: refuse)) }
                switch command {
                case .listBots: return .bots(bots: [bot])
                case .getBotStatus: return .botStatus(bots: [])
                case .putBot(let bot):
                    self.bot = bot
                    return .applied
                case .getBotMemory: break
                case .setBotFact(_, let factId, let text):
                    let fact = BotFact(id: factId ?? "f\(facts.count + 1)", text: text, learnedAt: "2026-10-27T09:00:00Z",
                                       thread: nil, place: nil, by: nil)
                    if let index = facts.firstIndex(where: { $0.id == factId }) { facts[index] = fact } else { facts.append(fact) }
                case .deleteBotFact(_, let factId): facts.removeAll { $0.id == factId }
                case .clearBotMemory: facts = []
                default: throw HerderError.Local(detail: "unexpected \(command)")
                }
                return .botMemory(botId: "trash-can", memory: BotMemory(facts: facts))
            }
            await bots.reload()
            let knowledge = BotKnowledge(bots: bots, botId: "trash-can", editable: editable)
            await knowledge.reload()
            return knowledge
        }
    }

    @Test func theRowSaysWhatTheBotKnows() async {
        let machine = Machine()
        let knowledge = await machine.knowledge()
        #expect(knowledge.summary == "No instructions · No facts yet")
        machine.bot.instructions = "Answer in British English."
        machine.facts = [BotFact(id: "f1", text: "Ada leads payments", learnedAt: "2026-10-27T09:00:00Z",
                                 thread: nil, place: nil, by: nil)]
        await knowledge.bots.reload()
        await knowledge.reload()
        #expect(knowledge.summary == "Instructions · 1 fact")
        #expect(knowledge.instructions == "Answer in British English.")
    }

    @Test func editedInstructionsSaveOnTheBotAndEmptyOnesAreNone() async {
        let machine = Machine()
        let knowledge = await machine.knowledge()
        #expect(!knowledge.changed)
        knowledge.instructions = "  Answer in British English.\n"
        #expect(knowledge.changed)
        await knowledge.saveInstructions()
        #expect(machine.bot.instructions == "Answer in British English.")
        #expect(!knowledge.changed)

        knowledge.instructions = String(repeating: "a", count: Int(maxBotInstructionsBytes()) + 1)
        #expect(knowledge.instructionsProblem == "1 bytes over the 8 KB limit.")
        await knowledge.saveInstructions()
        #expect(machine.bot.instructions == "Answer in British English.")

        knowledge.instructions = " "
        await knowledge.saveInstructions()
        #expect(machine.bot.instructions == nil)
    }

    @Test func factsAreAddedEditedInPlaceDeletedAndCleared() async {
        let machine = Machine()
        let knowledge = await machine.knowledge()
        #expect(await knowledge.setFact(nil, "Staging deploys go through make deploy-stg"))
        #expect(await knowledge.setFact(nil, "Ada leads payments"))
        #expect(await knowledge.setFact("f1", "Staging deploys go through make deploy-staging"))
        #expect(knowledge.list.map(\.text) == ["Staging deploys go through make deploy-staging", "Ada leads payments"])
        await knowledge.delete("f2")
        #expect(knowledge.list.map(\.id) == ["f1"])
        await knowledge.clear()
        #expect(knowledge.list.isEmpty)
        #expect(machine.sent.last == .clearBotMemory(botId: "trash-can"))
    }

    @Test func aRefusedFactSaysWhyAndKeepsTheList() async {
        let machine = Machine()
        let knowledge = await machine.knowledge()
        await knowledge.setFact(nil, "Ada leads payments")
        machine.refuse = "this looks like it holds a secret"
        #expect(!(await knowledge.setFact(nil, "xoxb-1234567890-abcdefghij")))
        #expect(knowledge.error == "this looks like it holds a secret")
        #expect(knowledge.list.map(\.text) == ["Ada leads payments"])
    }

    @Test func aMemberSeesBothAndChangesNeither() async {
        let machine = Machine()
        machine.facts = [BotFact(id: "f1", text: "Ada leads payments", learnedAt: "2026-10-27T09:00:00Z",
                                 thread: nil, place: nil, by: nil)]
        let knowledge = await machine.knowledge(editable: false)
        #expect(knowledge.list.count == 1)
        let before = machine.sent.count
        #expect(!(await knowledge.setFact(nil, "Bob is on call")))
        await knowledge.delete("f1")
        await knowledge.clear()
        knowledge.instructions = "Be terse."
        await knowledge.saveInstructions()
        #expect(machine.sent.count == before)
        #expect(machine.bot.instructions == nil)
    }

    @Test func aFactSaysWhereAndWhenItWasLearned() {
        let now = Timestamp.date("2026-10-30T09:00:00Z")!
        let learned = BotFact(id: "f1", text: "t", learnedAt: "2026-10-27T09:00:00Z", thread: Self.thread,
                              place: .channel(name: "general"), by: "Ada")
        #expect(BotKnowledge.learned(learned, now: now) == "Learned from Ada in #general, 3 days ago")
        var direct = learned
        direct.place = .direct
        #expect(BotKnowledge.learned(direct, now: now) == "Learned from Ada in a direct message, 3 days ago")
        let owner = BotFact(id: "f2", text: "t", learnedAt: "2026-10-30T08:59:30Z", thread: nil, place: nil, by: nil)
        #expect(BotKnowledge.learned(owner, now: now) == "Added by an owner, just now")
    }
}
