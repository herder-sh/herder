import Herder
import SwiftUI

/// Every bot on every machine this device owns: its kind and machine, how its connection
/// stands, how many people it answers and its default project. A bot opens as its page.
struct IntegrationsView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    @State private var bots: [HostId: MachineBots] = [:]
    @Environment(\.pages) private var pages

    var body: some View {
        let machines = fleet.botMachines
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                if machines.isEmpty {
                    Text("No machine you own is connected.").foregroundStyle(Theme.tertiary).padding(.top, 30)
                }
                ForEach(machines, id: \.hostId) { machine in
                    if let list = bots[machine.hostId] { section(machine, list) }
                }
                Text("Each bot is its own Slack app, run by one machine over Socket Mode. People on its list drive sessions from a DM, or by mentioning it in a channel.")
                    .font(.footnote).foregroundStyle(Theme.tertiary)
            }
            .pageColumn()
        }
        .background(Theme.background)
        .refreshable { await reload() }
        .navigationTitle("Integrations")
        #if os(iOS)
        .toolbar {
            Button("Add Bot", systemImage: "plus") { sheet = .addBot }
        }
        #endif
        .task(id: machines.map(\.hostId)) {
            bots = Dictionary(uniqueKeysWithValues: machines.map { ($0.hostId, bots[$0.hostId] ?? MachineBots(fleet: fleet, hostId: $0.hostId)) })
            await reload()
            // Statuses change with no event, so they are asked again while the list shows.
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(5))
                for list in bots.values { await list.reloadStatus() }
            }
        }
        .onChange(of: sheet) { if sheet == nil { Task { await reload() } } }
    }

    private func reload() async {
        for list in bots.values { await list.reload() }
    }

    @ViewBuilder private func section(_ machine: Machine, _ list: MachineBots) -> some View {
        SettingsList(title: machine.name) {
            switch list.bots {
            case .asking:
                ProgressView().frame(maxWidth: .infinity, minHeight: Theme.tallRowHeight)
            case .failed(let why):
                Text(why).font(.footnote).foregroundStyle(Theme.failure).padding(Theme.cellPadding)
            case .loaded(let entries) where entries.isEmpty:
                Text("No bots").font(.subheadline).foregroundStyle(Theme.tertiary)
                    .padding(.horizontal, Theme.cellPadding).frame(maxWidth: .infinity, minHeight: Theme.rowHeight, alignment: .leading)
            case .loaded(let entries):
                ForEach(Array(entries.enumerated()), id: \.element.botId) { index, bot in
                    if index > 0 { RowDivider() }
                    BotRow(bot: bot, machine: machine.name, status: list.statuses[bot.botId],
                           project: bot.defaults.projectId.map { fleet.projectName($0) }) {
                        pages.open(.bot(hostId: machine.hostId, botId: bot.botId))
                    }
                }
            }
        }
    }
}

/// A bot in a list: its kind, name and machine, the people it answers and its default project,
/// then how its connection stands.
struct BotRow: View {
    let bot: Bot
    let machine: String
    let status: BotStatus?
    /// Its default project's name; `nil` for chats.
    let project: String?
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 12) {
                BotMark(kind: bot.kind)
                VStack(alignment: .leading, spacing: 2) {
                    Text(bot.name).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text).lineLimit(1)
                    Text([bot.kind.label, machine, bot.allow.count == 1 ? "1 person" : "\(bot.allow.count) people",
                          project ?? "Chats"].joined(separator: " · "))
                        .font(.caption).foregroundStyle(Theme.tertiary).lineLimit(1)
                }
                Spacer(minLength: 8)
                if let status {
                    StatusChip(text: status.state.label, urgency: status.state.urgency)
                }
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
            }
            .padding(.horizontal, Theme.cellPadding)
            .padding(.vertical, 8)
            .frame(minHeight: Theme.tallRowHeight)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("bot-\(bot.botId)")
    }
}

/// A chat's mark: a speech bubble with the chat's initial.
struct BotMark: View {
    let kind: BotKind
    var size: CGFloat = 28

    var body: some View {
        Text(kind.label.prefix(1))
            .font(.system(size: size * 0.5, weight: .bold, design: .rounded))
            .foregroundStyle(.white)
            .frame(width: size, height: size)
            .background(kind == .slack ? Theme.accent : Theme.running, in: .rect(cornerRadius: size * 0.28))
            .accessibilityHidden(true)
    }
}

extension Fleet {
    /// A project's name, as its group lists it, or its id.
    func projectName(_ projectId: ProjectId) -> String {
        lists.projects.first { $0.projectId == projectId }?.name ?? projectId
    }
}

/// Adds a bot: the machine and the kind, the Slack app's manifest to copy, then its tokens.
struct AddBotSheet: View {
    let fleet: Fleet
    @State private var setup: BotSetup
    @Environment(\.dismiss) private var dismiss

    init(fleet: Fleet) {
        self.fleet = fleet
        _setup = State(initialValue: BotSetup(fleet: fleet))
    }

    var body: some View {
        @Bindable var setup = setup
        SheetScaffold(title: "Add Bot", subtitle: "Run a chat bot on one of your machines", height: 760) {
            Field(label: "Machine", hint: "It holds the bot’s connection and runs the sessions it starts.") {
                ChoiceChips(options: fleet.botMachines.map { (value: Optional($0.hostId), label: $0.name, detail: "") },
                            selection: $setup.hostId)
            }
            Field(label: "Kind") {
                ChoiceChips(options: [(value: BotKind.slack, label: "Slack", detail: "Socket Mode"),
                                      (value: BotKind.telegram, label: "Telegram", detail: "Bot API")],
                            selection: $setup.kind)
            }
            Field(label: "Name", hint: "As the chat shows the bot user, such as trash-can.") {
                InputBox(placeholder: "trash-can", text: $setup.name).accessibilityIdentifier("bot-name")
            }
            if setup.kind == .slack {
                Field(label: "1 · Create the Slack app",
                      hint: "At api.slack.com/apps choose **Create New App › From a manifest**, pick the workspace and paste this. Then install it to the workspace.") {
                    ManifestBox(text: BotSetup.slackManifest(name: setup.name))
                }
                Field(label: "2 · Bot token",
                      hint: "**OAuth & Permissions › Bot User OAuth Token**, starting xoxb-.") {
                    TokenBox(placeholder: "xoxb-…", text: $setup.botToken.text).accessibilityIdentifier("bot-token")
                }
                Field(label: "3 · App-level token",
                      hint: "**Basic Information › App-Level Tokens › Generate**, with the connections:write scope; it starts xapp-.") {
                    TokenBox(placeholder: "xapp-…", text: $setup.appToken.text).accessibilityIdentifier("bot-app-token")
                }
            } else {
                Field(label: "Bot token", hint: "From @BotFather.") {
                    TokenBox(placeholder: "123456:ABC…", text: $setup.botToken.text).accessibilityIdentifier("bot-token")
                }
            }
            Text("The tokens go into the machine’s vault as secrets of the set \(setup.botId.isEmpty ? "bot-‹name›" : setup.secretSet), and are never shown again.")
                .font(.footnote).foregroundStyle(Theme.tertiary)
            if let error = setup.error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure).accessibilityIdentifier("bot-error")
            }
        } footer: {
            ActionButton(title: "Add Bot", style: .primary) {
                if await setup.save() { dismiss() }
            }
            .disabled(setup.sending)
        }
    }
}

/// A token as it is typed: hidden, and never filled in again.
private struct TokenBox: View {
    let placeholder: String
    @Binding var text: String

    var body: some View {
        SecureField(placeholder, text: $text)
            .textFieldStyle(.plain)
            .font(Theme.mono)
            .foregroundStyle(Theme.text)
            .autocorrectionDisabled()
            #if os(iOS)
            .textInputAutocapitalization(.never)
            #endif
            .padding(12)
            .frame(minHeight: Theme.rowHeight)
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// The app manifest, selectable, with a button that copies it.
private struct ManifestBox: View {
    let text: String
    @State private var copied = false

    var body: some View {
        VStack(alignment: .trailing, spacing: 0) {
            PillButton(title: copied ? "Copied" : "Copy", symbol: copied ? "checkmark" : "doc.on.doc") {
                Clipboard.string = text
                copied = true
            }
            .padding(8)
            ScrollView {
                Text(text)
                    .font(Theme.monoSmall)
                    .foregroundStyle(Theme.secondary)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 12)
                    .padding(.bottom, 12)
            }
            .frame(maxHeight: 180)
        }
        .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
        .onChange(of: text) { copied = false }
    }
}

/// A machine's bots on its page, each opening as its own page.
struct MachineBotsSection: View {
    let fleet: Fleet
    let machine: Machine
    @State private var bots: MachineBots
    @Environment(\.pages) private var pages

    init(fleet: Fleet, machine: Machine) {
        self.fleet = fleet
        self.machine = machine
        _bots = State(initialValue: MachineBots(fleet: fleet, hostId: machine.hostId))
    }

    var body: some View {
        SettingsList(title: "Bots") {
            ForEach(Array(bots.list.enumerated()), id: \.element.botId) { index, bot in
                if index > 0 { RowDivider() }
                BotRow(bot: bot, machine: machine.name, status: bots.statuses[bot.botId],
                       project: bot.defaults.projectId.map { fleet.projectName($0) }) {
                    pages.open(.bot(hostId: machine.hostId, botId: bot.botId))
                }
            }
            if case .failed(let why) = bots.bots {
                Text(why).font(.footnote).foregroundStyle(Theme.failure).padding(Theme.cellPadding)
            } else if bots.list.isEmpty {
                Text("No bots. Add one under Integrations.").font(.subheadline).foregroundStyle(Theme.tertiary)
                    .padding(.horizontal, Theme.cellPadding).frame(maxWidth: .infinity, minHeight: Theme.rowHeight, alignment: .leading)
            }
        }
        .task { await bots.reload() }
    }
}

/// A session's linked chat thread, as a small badge that opens it, when the chat gave a link.
struct ThreadBadge: View {
    let threads: [ThreadLink]

    var body: some View {
        if let thread = threads.last {
            let label = Label(thread.kind.label, systemImage: "bubble.left.and.text.bubble.right")
                .labelStyle(.titleAndIcon)
                .font(.caption2.weight(.semibold))
                .foregroundStyle(Theme.secondary)
                .padding(.horizontal, 6).padding(.vertical, 1)
                .background(Theme.raised, in: .capsule)
                .lineLimit(1)
            if let url = thread.url.flatMap(URL.init) {
                Link(destination: url) { label }
                    .buttonStyle(.plain)
                    .help("Open the linked \(thread.kind.label) thread")
            } else {
                label.help("Linked to a \(thread.kind.label) thread")
            }
        }
    }
}
