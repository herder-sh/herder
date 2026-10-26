import Foundation
import Herder
@testable import HerderKit
import Testing

@MainActor
struct BotsTests {
    static let defaults = BotDefaults(projectId: nil, provider: nil, accountIds: [], model: nil, permissionMode: nil)

    static func bot(_ id: String, allow: [BotUser] = []) -> Bot {
        Bot(botId: id, kind: .slack, name: id, token: "bot-\(id)/BOT_TOKEN", appToken: "bot-\(id)/APP_TOKEN",
            allow: allow, defaults: defaults, channelBindings: [])
    }

    /// A machine that keeps the bots it is put, and says how each stands.
    @MainActor final class Machine {
        var bots: [Bot] = [BotsTests.bot("trash-can")]
        var states: [BotId: BotState] = ["trash-can": .waitingForVault]
        var sent: [CommandBody] = []

        func model() -> MachineBots {
            MachineBots(hostId: "devbox") { [self] command in
                sent.append(command)
                switch command {
                case .listBots: return .bots(bots: bots)
                case .getBotStatus:
                    return .botStatus(bots: bots.map { BotStatus(botId: $0.botId, state: states[$0.botId] ?? .connecting, lastError: nil) })
                case .putBot(let bot):
                    bots.removeAll { $0.botId == bot.botId }
                    bots.append(bot)
                    return .applied
                case .removeBot(let botId):
                    bots.removeAll { $0.botId == botId }
                    return .applied
                default:
                    throw HerderError.Local(detail: "unexpected \(command)")
                }
            }
        }
    }

    @Test func listsTheMachinesBotsWithHowEachStands() async {
        let machine = Machine()
        let bots = machine.model()
        await bots.reload()
        #expect(bots.list.map(\.botId) == ["trash-can"])
        #expect(bots.statuses["trash-can"]?.state == .waitingForVault)

        machine.states["trash-can"] = .connected
        await bots.reloadStatus()
        #expect(bots.statuses["trash-can"]?.state == .connected)
    }

    @Test func aChangePutsTheWholeBotAndListsAgain() async throws {
        let machine = Machine()
        let bots = machine.model()
        await bots.reload()
        let person = BotUser(externalUserId: "U012AB3CD", displayName: "Sam", dm: true, channels: false, approve: true, notify: false)
        try await bots.update("trash-can") {
            $0.allow.append(person)
            $0.defaults.projectId = "github.com/acme/api"
        }
        #expect(machine.bots[0].allow == [person])
        #expect(bots.bot("trash-can")?.defaults.projectId == "github.com/acme/api")
        #expect(machine.sent.suffix(3).first == .putBot(bot: machine.bots[0]))

        try await bots.remove("trash-can")
        #expect(machine.bots.isEmpty)
        #expect(bots.list.isEmpty)
    }

    @Test func aRefusedListSaysWhy() async {
        let bots = MachineBots(hostId: "devbox") { _ in
            throw HerderError.Rejected(info: ErrorInfo(code: .unsupported, message: "a vault runs no bots"))
        }
        await bots.reload()
        guard case .failed(let why) = bots.bots else { Issue.record("not failed"); return }
        #expect(why.contains("a vault runs no bots"))
    }

    /// What a daemon older than a command answers it with.
    static let unknownCommand = HerderError.Rejected(info: ErrorInfo(
        code: .badRequest, message: "invalid message: unknown variant `list_bots`, expected one of `hello`, `list_sessions`"))

    @Test func aCommandTheMachineDoesNotKnowAsksForANewerHerder() {
        #expect(describe(Self.unknownCommand) == "This needs a newer herder on that machine.")
        let other = HerderError.Rejected(info: ErrorInfo(code: .badRequest, message: "invalid message: missing field `name`"))
        #expect(!other.needsNewerHerder)
        #expect(describe(other) == "invalid message: missing field `name`")
    }

    @Test func aMachineTooOldForBotsSaysSoInOneLine() async throws {
        let bots = MachineBots(hostId: "draugelis") { _ in throw Self.unknownCommand }
        await bots.reload()
        #expect(bots.outdated)
        let note = try #require(BotsNote(machine: "draugelis", bots: bots))
        #expect(note.outdated)
        #expect(note.text == "draugelis runs an older herder; bots need 0.12.0.")
    }

    @Test func theTableHasARowPerBotAndNoneForAMachineWithout() async {
        let list = Machine().model()
        await list.reload()
        let empty = MachineBots(hostId: "trash") { _ in .bots(bots: []) }
        await empty.reload()
        let rows = BotTableRow.rows(
            [machine("devbox", name: "devbox", sessions: []), machine("trash", name: "trash", sessions: [])],
            ["devbox": list, "trash": empty], projectName: { $0 })
        #expect(rows.map(\.id) == ["devbox/trash-can"])
        #expect(rows[0].status?.state == .waitingForVault)
        // The machine retries on its own: neutral, not amber.
        #expect(rows[0].status?.state.urgency == .quiet)
    }

    @Test func theHeaderCountsTheBotsAndHowEachStandsLeavingOutZeros() async {
        let fleet = FleetBots()
        fleet.keep(["devbox", "old"]) { hostId in
            hostId == "old" ? MachineBots(hostId: hostId) { _ in throw Self.unknownCommand } : Machine().model()
        }
        #expect(fleet.status == "")
        await fleet.reload()
        #expect(fleet.status == "1 bot · 1 waiting for the vault · 1 machine needs a newer herder")
        fleet.keep([]) { _ in Machine().model() }
        #expect(fleet.status == "")
    }

    @Test func addBotOffersOnlyMachinesThatCanRunABotAndSaysWhyNot() async {
        let targets = BotTarget.all([
            machine("member", name: "member", sessions: [], role: .member),
            machine("off", name: "off", sessions: [], connection: .disconnected(error: "offline")),
            machine("old", name: "draugelis", sessions: []),
            machine("novault", name: "laptop", sessions: []),
            machine("devbox", name: "devbox", sessions: []),
        ])
        let setup = BotSetup(hostId: nil, send: { host, _ in
            if host == "old" { throw Self.unknownCommand }
            return .bots(bots: [])
        }, vaultOf: { host in
            if host == "novault" { throw HerderError.Local(detail: "The machine is linked to no vault.") }
            return "vault"
        })
        await setup.check(targets)
        #expect(setup.targets.map(\.problem) == [
            "Only its owners add bots.", "Not connected.", "Runs an older herder; bots need 0.12.0.",
            "The machine is linked to no vault.", nil,
        ])
        #expect(setup.hostId == "devbox")
        setup.hostId = "old"
        #expect(setup.problem == "Runs an older herder; bots need 0.12.0.")
    }

    @Test func thePrimaryButtonNamesTheBot() {
        let setup = Sent().setup()
        #expect(setup.actionTitle == "Add Bot")
        setup.name = " trash-can "
        #expect(setup.actionTitle == "Add trash-can bot")
    }

    /// Records what Add Bot sends to the vault and the machine.
    @MainActor final class Sent {
        var commands: [(HostId, CommandBody)] = []
        var sets: [SecretSet] = []

        func setup() -> BotSetup {
            BotSetup(hostId: "devbox", send: { [self] host, command in
                commands.append((host, command))
                if case .listSecrets = command { return .secrets(sets: sets, secrets: []) }
                return .applied
            }, vaultOf: { _ in "vault" })
        }
    }

    @Test func addingABotKeepsItsTokensInTheVaultAndPutsTheBotOnItsMachine() async {
        let sent = Sent()
        let setup = sent.setup()
        setup.name = "Trash Can"
        setup.botToken.text = "xoxb-1"
        setup.appToken.text = "xapp-2"
        #expect(await setup.save())
        #expect(setup.botToken.isEmpty && setup.appToken.isEmpty)
        let commands = sent.commands
        #expect(commands.map(\.0) == ["vault", "vault", "vault", "vault", "vault", "devbox"])
        #expect(commands[1].1 == .createSecretSet(set: "bot-trash-can"))
        guard case .putSecret(let set, let name, _, let value) = commands[2].1 else { Issue.record("no bot token"); return }
        #expect([set, name, value] == ["bot-trash-can", "BOT_TOKEN", "xoxb-1"])
        guard case .putSecret(_, let app, _, let appValue) = commands[3].1 else { Issue.record("no app token"); return }
        #expect([app, appValue] == ["APP_TOKEN", "xapp-2"])
        #expect(commands[4].1 == .grantSecretSet(set: "bot-trash-can", hostId: "devbox", hostUse: true))
        #expect(commands[5].1 == .putBot(bot: Bot(
            botId: "trash-can", kind: .slack, name: "Trash Can", token: "bot-trash-can/BOT_TOKEN",
            appToken: "bot-trash-can/APP_TOKEN", allow: [], defaults: Self.defaults, channelBindings: [])))
    }

    @Test func addingABotAgainReusesItsSet() async {
        let sent = Sent()
        sent.sets = [SecretSet(name: "bot-trash-can", hosts: [], hostUse: ["devbox"])]
        let setup = sent.setup()
        setup.name = "trash-can"
        setup.botToken.text = "xoxb-1"
        setup.appToken.text = "xapp-2"
        #expect(await setup.save())
        #expect(!sent.commands.contains { $0.1 == .createSecretSet(set: "bot-trash-can") })
    }

    @Test func aSlackBotNeedsBothTokensAndSendsNothingWithout() async {
        let sent = Sent()
        let setup = sent.setup()
        setup.name = "trash-can"
        setup.botToken.text = "xapp-wrong"
        #expect(!(await setup.save()))
        #expect(setup.error == "The bot token starts with xoxb-.")
        setup.botToken.text = "xoxb-1"
        #expect(setup.problem == "The app-level token starts with xapp-.")
        setup.kind = .telegram
        #expect(setup.problem == nil)
        #expect(sent.commands.isEmpty)
    }

    @Test func aVaultThatCannotBeReachedKeepsTheBotOffTheMachine() async {
        let setup = BotSetup(hostId: "devbox", send: { _, _ in .applied }, vaultOf: { _ in
            throw HerderError.Local(detail: "The machine is linked to no vault")
        })
        setup.name = "trash-can"
        setup.botToken.text = "xoxb-1"
        setup.appToken.text = "xapp-2"
        #expect(!(await setup.save()))
        #expect(setup.error?.contains("linked to no vault") == true)
        #expect(setup.botToken.isEmpty)
    }

    @Test func theBotsIdAndSetComeFromItsName() {
        #expect(BotSetup.slug("Trash Can!") == "trash-can")
        #expect(BotSetup.slug("  trip1 -- team ") == "trip1-team")
        #expect(VaultSecrets.setNameProblem("bot-" + BotSetup.slug("Trip1 Team")) == nil)
    }

    @Test func theManifestRunsOverSocketModeWithTheScopesTheDaemonUses() {
        let manifest = BotSetup.slackManifest(name: "trash-can")
        #expect(manifest.contains("display_name: trash-can"))
        #expect(manifest.contains("socket_mode_enabled: true"))
        for needed in ["app_mentions:read", "chat:write", "files:read", "files:write", "im:history", "- app_mention", "- message.im"] {
            #expect(manifest.contains(needed), "\(needed)")
        }
    }

    @Test func aPersonIsAddedByMemberIdOnce() throws {
        var draft = BotPersonDraft(memberId: " u012ab3cd ", name: "Sam")
        let sam = try #require(try draft.person(among: []))
        #expect(sam.externalUserId == "U012AB3CD")
        #expect(sam.dm && sam.channels && sam.approve && !sam.notify)
        #expect(throws: (any Error).self) { try draft.person(among: [sam]) }
        draft.memberId = "U01-2"
        #expect(throws: (any Error).self) { try draft.person(among: []) }
        draft.name = ""
        #expect(try draft.person(among: []) == nil)
    }

    @Test func aLinkedSessionCarriesItsThreadsToItsRow() {
        var script = Script("01A")
        var host = machine("host-a", name: "a", sessions: ["01A"])
        let link = ThreadLink(kind: .slack, botId: "trash-can", channelId: "D1", threadId: "1.2",
                              url: "https://acme.slack.com/archives/D1/p12")
        host.sessions[0].threads = [link]
        let lists = Lists(machines: [host], sessions: [script.key: script.model([created()])])
        let summary = (lists.projects.flatMap(\.sessions) + lists.chats).first { $0.key == script.key }
        #expect(summary?.threads == [link])
    }
}
