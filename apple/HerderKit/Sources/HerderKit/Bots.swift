import Foundation
import Herder
import Observation

/// A machine's chat bots, for its owners: its config's bots and how each one's connection
/// stands. Each change goes to the machine, then both are asked again.
@MainActor @Observable
final class MachineBots {
    typealias Send = @MainActor (CommandBody) async throws -> CommandResult

    let hostId: HostId
    private let send: Send
    private(set) var bots: Load<[Bot]> = .asking
    private(set) var statuses: [BotId: BotStatus] = [:]
    /// Whether the machine's herder is too old to know bots.
    private(set) var outdated = false

    init(hostId: HostId, send: @escaping Send) {
        self.hostId = hostId
        self.send = send
    }

    /// The bots of `hostId`, sent through `fleet`.
    convenience init(fleet: Fleet, hostId: HostId) {
        let client = fleet.client
        self.init(hostId: hostId) { try await client.send(hostId: hostId, command: $0) }
    }

    var list: [Bot] {
        if case .loaded(let bots) = bots { bots } else { [] }
    }

    func bot(_ botId: BotId) -> Bot? {
        list.first { $0.botId == botId }
    }

    /// Asks the machine for its bots, and how each one's connection stands.
    func reload() async {
        do {
            guard case .bots(let bots) = try await send(.listBots) else {
                throw HerderError.Local(detail: "the machine did not list its bots")
            }
            self.bots = .loaded(bots)
            outdated = false
        } catch {
            bots = .failed(describe(error))
            outdated = (error as? HerderError)?.needsNewerHerder == true
            return
        }
        await reloadStatus()
    }

    /// Asks only how each bot's connection stands.
    func reloadStatus() async {
        guard case .botStatus(let statuses) = try? await send(.getBotStatus) else { return }
        self.statuses = Dictionary(statuses.map { ($0.botId, $0) }, uniquingKeysWith: { $1 })
    }

    /// Adds `bot`, or replaces the one with its id.
    func put(_ bot: Bot) async throws {
        _ = try await send(.putBot(bot: bot))
        await reload()
    }

    /// Changes the bot `botId` as the machine has it now.
    func update(_ botId: BotId, _ change: (inout Bot) -> Void) async throws {
        guard var bot = bot(botId) else { throw HerderError.Local(detail: "the bot is gone") }
        change(&bot)
        try await put(bot)
    }

    func remove(_ botId: BotId) async throws {
        _ = try await send(.removeBot(botId: botId))
        await reload()
    }
}

/// Every owned machine's bots, kept while the app runs so Integrations and its header share
/// them.
@MainActor @Observable
final class FleetBots {
    private(set) var lists: [HostId: MachineBots] = [:]

    /// Keeps a list for each of `hostIds` and none other, making those it lacks with `make`.
    func keep(_ hostIds: [HostId], make: (HostId) -> MachineBots) {
        lists = Dictionary(uniqueKeysWithValues: hostIds.map { ($0, lists[$0] ?? make($0)) })
    }

    func reload() async {
        for list in lists.values { await list.reload() }
    }

    func reloadStatus() async {
        for list in lists.values { await list.reloadStatus() }
    }

    /// "2 bots · 2 waiting for the vault": how many, then how many stand each way but connected,
    /// then the machines too old for bots; zeros left out.
    /// Empty until every machine has answered.
    var status: String {
        if lists.isEmpty || lists.values.contains(where: { if case .asking = $0.bots { true } else { false } }) { return "" }
        let bots = lists.values.flatMap { list in list.list.map { list.statuses[$0.botId]?.state } }
        let outdated = lists.values.filter(\.outdated).count
        var parts = [bots.isEmpty ? "No bots yet" : bots.count == 1 ? "1 bot" : "\(bots.count) bots"]
        for state in [BotState.connecting, .waitingForVault, .tokenRejected, .unsupported] {
            let count = bots.filter { $0 == state }.count
            if count > 0 { parts.append("\(count) \(state.label.lowercased())") }
        }
        if outdated > 0 { parts.append(outdated == 1 ? "1 machine needs a newer herder" : "\(outdated) machines need a newer herder") }
        return parts.joined(separator: " · ")
    }
}

/// The herder release that runs bots.
let botsRelease = "0.12.0"

/// A machine as the place to add a bot to, with why it can't be, when it can't.
struct BotTarget: Identifiable, Equatable {
    var id: HostId { hostId }
    let hostId: HostId
    let name: String
    var problem: String?

    /// Every daemon this device is paired with, those it can't add a bot to now with why.
    static func all(_ machines: [Machine]) -> [BotTarget] {
        machines.filter { $0.hosts.isEmpty && $0.vault == nil }.map { machine in
            BotTarget(hostId: machine.hostId, name: machine.name, problem:
                machine.role != .owner ? "Only its owners add bots."
                : machine.connection != .connected ? "Not connected."
                : nil)
        }
    }
}

extension Fleet {
    /// The machines whose bots this device can list: connected daemons it owns, not vaults.
    var botMachines: [Machine] {
        machines.filter { $0.role == .owner && $0.connection == .connected && $0.hosts.isEmpty }
    }

    /// The paired vault `hostId` keeps its machine-level secrets in, which this device owns.
    func secretsVault(of hostId: HostId) async throws -> HostId {
        switch try await backupLink(of: hostId) {
        case .vault:
            throw HerderError.Local(detail: "A vault runs no bots.")
        case .nowhere:
            throw HerderError.Local(detail: "The machine is linked to no vault, which keeps the bot’s tokens. Link it to one under Backup on its page.")
        case .linked(let link):
            guard link.secrets else {
                throw HerderError.Local(detail: "The machine fetches no secrets from its vault. Turn on Secrets under Backup on its page.")
            }
            guard let vault = pairedVault(link), vault.role == .owner else {
                throw HerderError.Local(detail: "Pair this device with the machine’s vault, at \(link.address), as its owner.")
            }
            return vault.hostId
        }
    }
}

extension BotKind {
    var label: String {
        switch self {
        case .slack: "Slack"
        case .telegram: "Telegram"
        case .unknown: "Chat"
        }
    }
}

extension BotState {
    var label: String {
        switch self {
        case .connecting: "Connecting"
        case .connected: "Connected"
        case .waitingForVault: "Waiting for the vault"
        case .tokenRejected: "Token rejected"
        case .unsupported: "Not supported here"
        case .unknown: "Unknown"
        }
    }

    /// The machine retries while it waits for the vault, so that is not the user's to act on.
    var urgency: Urgency {
        switch self {
        case .connected: .done
        case .connecting: .running
        case .waitingForVault, .unknown: .quiet
        case .tokenRejected, .unsupported: .failing
        }
    }

    /// What to do about it, when anything.
    var fix: String? {
        switch self {
        case .waitingForVault: "The machine cannot read the bot’s tokens from its vault. It retries; check the vault is up and its set is granted to the machine."
        case .tokenRejected: "The chat refused the bot’s tokens. Add the bot again with new ones."
        case .unsupported: "This machine’s herder cannot run bots of this kind yet."
        case .connecting, .connected, .unknown: nil
        }
    }
}

/// Adding a bot to a machine: its machine, kind and name, then its tokens. Saving puts the
/// tokens into the machine's vault as machine-level secrets, in a set of the bot's own granted
/// to the machine for its own use, then the bot on the machine. The tokens leave the form before
/// anything is sent, so they are never kept nor shown again.
@MainActor @Observable
final class BotSetup {
    typealias Send = @MainActor (HostId, CommandBody) async throws -> CommandResult

    private let send: Send
    /// The vault that keeps a machine's secrets.
    private let vaultOf: @MainActor (HostId) async throws -> HostId
    var hostId: HostId?
    /// The machines it may go on, each with why it can't, once asked.
    private(set) var targets: [BotTarget] = []
    var kind = BotKind.slack
    var name = ""
    /// Slack's bot token (`xoxb-`), or Telegram's.
    var botToken = SecretInput()
    /// Slack's app-level token (`xapp-`).
    var appToken = SecretInput()
    private(set) var error: String?
    private(set) var sending = false

    static let botTokenName = "BOT_TOKEN"
    static let appTokenName = "APP_TOKEN"

    init(hostId: HostId?, send: @escaping Send, vaultOf: @escaping @MainActor (HostId) async throws -> HostId) {
        self.hostId = hostId
        self.send = send
        self.vaultOf = vaultOf
    }

    convenience init(fleet: Fleet) {
        let client = fleet.client
        self.init(hostId: nil,
                  send: { try await client.send(hostId: $0, command: $1) },
                  vaultOf: { try await fleet.secretsVault(of: $0) })
    }

    /// Asks each of `targets` that may run a bot whether its herder knows bots and its vault
    /// can keep the tokens, then picks the first that can when none is picked.
    func check(_ targets: [BotTarget]) async {
        var checked = targets
        for index in checked.indices where checked[index].problem == nil {
            let hostId = checked[index].hostId
            do {
                _ = try await send(hostId, .listBots)
                _ = try await vaultOf(hostId)
            } catch {
                checked[index].problem = (error as? HerderError)?.needsNewerHerder == true
                    ? "Runs an older herder; bots need \(botsRelease)." : describe(error)
            }
        }
        self.targets = checked
        if !checked.contains(where: { $0.hostId == hostId && $0.problem == nil }) {
            hostId = checked.first { $0.problem == nil }?.hostId
        }
    }

    /// What the primary button does: "Add trash-can bot".
    var actionTitle: String {
        let name = name.trimmingCharacters(in: .whitespaces)
        return name.isEmpty ? "Add Bot" : "Add \(name) bot"
    }

    /// The bot's id, from its name: lowercase letters, digits and single hyphens.
    var botId: String { Self.slug(name) }

    /// The vault set that keeps its tokens.
    var secretSet: String { "bot-\(botId)" }

    var problem: String? {
        guard let hostId else { return "Pick the machine that runs the bot." }
        if let problem = targets.first(where: { $0.hostId == hostId })?.problem { return problem }
        if botId.isEmpty { return "Name the bot, as the chat shows it, such as trash-can." }
        if kind == .slack {
            if !botToken.text.hasPrefix("xoxb-") { return "The bot token starts with xoxb-." }
            if !appToken.text.hasPrefix("xapp-") { return "The app-level token starts with xapp-." }
        } else if botToken.isEmpty {
            return "Enter the bot’s token."
        }
        return nil
    }

    /// Saves the tokens and the bot; whether the machine kept it. The tokens are cleared
    /// first either way.
    func save() async -> Bool {
        if let problem {
            error = problem
            return false
        }
        guard let hostId else { return false }
        let tokens = [(Self.botTokenName, botToken.text)] + (kind == .slack ? [(Self.appTokenName, appToken.text)] : [])
        botToken = SecretInput()
        appToken = SecretInput()
        error = nil
        sending = true
        defer { sending = false }
        let name = name.trimmingCharacters(in: .whitespaces)
        let set = secretSet
        do {
            let vault = try await vaultOf(hostId)
            guard case .secrets(let sets, _) = try await send(vault, .listSecrets) else {
                throw HerderError.Local(detail: "the vault did not list its secrets")
            }
            if !sets.contains(where: { $0.name == set }) {
                _ = try await send(vault, .createSecretSet(set: set))
            }
            for (secret, value) in tokens {
                _ = try await send(vault, .putSecret(
                    set: set, name: secret, description: "\(kind.label) \(secret == Self.botTokenName ? "bot" : "app-level") token of the bot \(name)",
                    value: value))
            }
            _ = try await send(vault, .grantSecretSet(set: set, hostId: hostId, hostUse: true))
            _ = try await send(hostId, .putBot(bot: Bot(
                botId: botId, kind: kind, name: name,
                token: "\(set)/\(Self.botTokenName)",
                appToken: kind == .slack ? "\(set)/\(Self.appTokenName)" : nil,
                allow: [], defaults: BotDefaults(projectId: nil, provider: nil, accountIds: [], model: nil, permissionMode: nil),
                channelBindings: [])))
            return true
        } catch {
            self.error = describe(error)
            return false
        }
    }

    /// Where Slack creates an app from a manifest.
    nonisolated static let slackAppsURL = URL(string: "https://api.slack.com/apps?new_app=1")
    /// Telegram's bot that makes bots.
    nonisolated static let botFatherURL = URL(string: "https://t.me/BotFather")

    /// Lowercase letters and digits of `name`, with single hyphens for whatever is between them.
    nonisolated static func slug(_ name: String) -> String {
        let words = name.lowercased().split { !(("a"..."z").contains($0) || ("0"..."9").contains($0)) }
        return String(words.joined(separator: "-").prefix(48))
    }

    /// The Slack app manifest to create the bot's app from: its bot user, the scopes and events
    /// the daemon uses, interactivity for its buttons, and Socket Mode, so the machine needs no
    /// public address.
    nonisolated static func slackManifest(name: String) -> String {
        let name = name.trimmingCharacters(in: .whitespaces).isEmpty ? "herder" : name.trimmingCharacters(in: .whitespaces)
        return """
        display_information:
          name: \(name)
          description: Drives herder sessions from Slack
        features:
          app_home:
            messages_tab_enabled: true
            messages_tab_read_only_enabled: false
          bot_user:
            display_name: \(name)
            always_online: true
        oauth_config:
          scopes:
            bot:
              - app_mentions:read
              - channels:history
              - chat:write
              - files:read
              - files:write
              - groups:history
              - im:history
              - im:write
              - mpim:history
        settings:
          event_subscriptions:
            bot_events:
              - app_mention
              - message.im
          interactivity:
            is_enabled: true
          socket_mode_enabled: true
        """
    }
}

/// A person a bot answers, being added: their Slack member ID and name.
struct BotPersonDraft: Equatable {
    var memberId = ""
    var name = ""

    /// The person, with every switch but Notify on; `nil` until both are filled.
    func person(among people: [BotUser]) throws -> BotUser? {
        let id = memberId.trimmingCharacters(in: .whitespaces).uppercased()
        let name = name.trimmingCharacters(in: .whitespaces)
        guard !id.isEmpty, !name.isEmpty else { return nil }
        guard id.allSatisfy({ ("A"..."Z").contains($0) || ("0"..."9").contains($0) }) else {
            throw HerderError.Local(detail: "A member ID is letters and digits, such as U012AB3CD: Slack shows it in their profile, under ⋯ › Copy member ID.")
        }
        if people.contains(where: { $0.externalUserId == id }) {
            throw HerderError.Local(detail: "\(id) is on the list already.")
        }
        return BotUser(externalUserId: id, displayName: name, dm: true, channels: true, approve: true, notify: false)
    }
}
