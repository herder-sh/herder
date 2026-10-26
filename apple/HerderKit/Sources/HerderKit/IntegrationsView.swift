import Herder
import SwiftUI

/// A bot as a row of the Integrations table.
struct BotTableRow: Identifiable {
    var id: String { "\(hostId)/\(bot.botId)" }
    let hostId: HostId
    let machine: String
    let bot: Bot
    let status: BotStatus?
    /// Its default project's name; `nil` for chats.
    let project: String?

    /// Machines with no bot have no row.
    @MainActor static func rows(_ machines: [Machine], _ bots: [HostId: MachineBots], projectName: (ProjectId) -> String) -> [BotTableRow] {
        machines.flatMap { machine in
            (bots[machine.hostId]?.list ?? []).map { bot in
                BotTableRow(hostId: machine.hostId, machine: machine.name, bot: bot,
                            status: bots[machine.hostId]?.statuses[bot.botId],
                            project: bot.defaults.projectId.map(projectName))
            }
        }
    }

    /// Connected first, then those on their way, then those that need something.
    var statusRank: Int {
        switch status?.state {
        case .connected: 0
        case .connecting: 1
        case nil, .unknown: 2
        case .waitingForVault: 3
        case .tokenRejected, .unsupported: 4
        }
    }
}

/// Every bot on every machine this device owns, as one table: its kind and machine, how its
/// connection stands, how many people it answers and its default project. A bot opens as its
/// page. Machines that can't list their bots say why under it, in a line each.
struct IntegrationsView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    /// The table's width, which decides its columns.
    @State private var width: CGFloat = 0
    @Environment(\.pages) private var pages

    var body: some View {
        let machines = fleet.botMachines
        let bots = fleet.bots.lists
        let rows = BotTableRow.rows(machines, bots, projectName: fleet.projectName)
        let asking = machines.contains { bots[$0.hostId].map { if case .asking = $0.bots { true } else { false } } ?? true }
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if !rows.isEmpty {
                    SortableTable("bots", rows: rows, sortedBy: TableSort(column: "Bot"), columns: columns) {
                        pages.open(.bot(hostId: $0.hostId, botId: $0.bot.botId))
                    }
                } else if asking {
                    ProgressView().frame(maxWidth: .infinity, minHeight: 120)
                } else {
                    BotsEmptyState(canAdd: machines.contains { !(bots[$0.hostId]?.outdated ?? false) }) { sheet = .addBot }
                }
                let notes = machines.compactMap { machine in bots[machine.hostId].flatMap { BotsNote(fleet: fleet, machine: machine, bots: $0) } }
                if !notes.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(notes, id: \.machine) { $0 }
                    }
                }
            }
            .pageColumn(width: .infinity)
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
        .background(Theme.background)
        .refreshable { await fleet.bots.reload() }
        .navigationTitle("Integrations")
        #if os(iOS)
        .toolbar {
            Button("Add Bot", systemImage: "plus") { sheet = .addBot }
        }
        #endif
        .task(id: machines.map(\.hostId)) {
            fleet.bots.keep(machines.map(\.hostId)) { MachineBots(fleet: fleet, hostId: $0) }
            await fleet.bots.reload()
            // Statuses change with no event, so they are asked again while the list shows.
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(5))
                await fleet.bots.reloadStatus()
            }
        }
        .onChange(of: sheet) { if sheet == nil { Task { await fleet.bots.reload() } } }
    }

    /// Too narrow for every column: the bot's name, its kind and machine under it, and its status.
    /// Every column takes about 630 pt and the bot's name wants 200 more.
    private var compact: Bool { width < 860 }

    private var columns: [SortableColumn<BotTableRow>] {
        var columns: [SortableColumn<BotTableRow>] = [
            SortableColumn("Bot", value: \.bot.name) { row in
                HStack(spacing: 10) {
                    BotMark(kind: row.bot.kind, size: 22)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(row.bot.name).fontWeight(.semibold).lineLimit(1)
                        if compact {
                            Text([row.bot.kind.label, row.machine].joined(separator: " · "))
                                .font(Theme.meta).foregroundStyle(Theme.tertiary).lineLimit(1)
                        }
                    }
                }
                .accessibilityIdentifier("bot-\(row.bot.botId)")
            },
        ]
        if !compact {
            columns += [
                SortableColumn("Kind", width: 80, value: \.bot.kind.label) { row in
                    Text(row.bot.kind.label).foregroundStyle(Theme.secondary).lineLimit(1)
                },
                SortableColumn("Machine", width: 150, value: \.machine) { row in
                    Text(row.machine).foregroundStyle(Theme.secondary).lineLimit(1)
                },
            ]
        }
        columns.append(SortableColumn("Status", width: compact ? 190 : 170, value: \.statusRank) { row in
            if let status = row.status {
                StatusChip(text: status.state.label, urgency: status.state.urgency)
            } else {
                Text("–").foregroundStyle(Theme.tertiary)
            }
        })
        guard !compact else { return columns }
        columns += [
            SortableColumn("People", width: 60, alignment: .trailing, value: \.bot.allow.count) { row in
                Text("\(row.bot.allow.count)").monospacedDigit().foregroundStyle(Theme.secondary)
            },
            SortableColumn("Project", width: 170, value: { $0.project ?? "" }) { row in
                Text(row.project ?? "Chats").foregroundStyle(row.project == nil ? Theme.tertiary : Theme.secondary).lineLimit(1)
            },
        ]
        return columns
    }
}

/// Why a machine's bots are not in the table, in one calm line: its herder is too old for bots,
/// or it refused to list them.
struct BotsNote: View {
    let fleet: Fleet
    let hostId: HostId
    let text: String
    /// Whether the machine's herder is too old, which updating it fixes.
    let outdated: Bool

    init?(fleet: Fleet, machine: Machine, bots: MachineBots) {
        guard case .failed(let why) = bots.bots else { return nil }
        self.fleet = fleet
        hostId = machine.hostId
        outdated = bots.outdated
        text = outdated ? "\(machine.name) runs an older herder; bots need \(botsRelease)." : "\(machine.name) did not list its bots: \(why)"
    }

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: outdated ? "arrow.up.circle" : "exclamationmark.circle")
                .foregroundStyle(outdated ? Theme.secondary : Theme.failure)
            Text(text).font(.subheadline).foregroundStyle(Theme.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 8)
            if outdated { HerderUpdateButton(fleet: fleet, hostId: hostId) }
        }
        .padding(.horizontal, 8)
        .frame(minHeight: 36)
        .accessibilityElement(children: .combine)
    }
}

/// A pill that copies `text`, and says so for a moment.
struct CopyPill: View {
    let title: String
    let text: String
    var prominent = false
    @State private var copied = false

    var body: some View {
        PillButton(title: copied ? "Copied" : title, symbol: copied ? "checkmark" : "doc.on.doc", prominent: prominent) {
            Clipboard.string = text
            copied = true
        }
        .fixedSize()
        .task(id: copied) {
            guard copied else { return }
            try? await Task.sleep(for: .seconds(1.5))
            copied = false
        }
    }
}

/// Integrations with no bot: what a bot is, and the button that adds one.
struct BotsEmptyState: View {
    /// Whether a machine could run one.
    let canAdd: Bool
    let add: () -> Void

    var body: some View {
        VStack(spacing: 12) {
            HStack(spacing: -6) {
                BotMark(kind: .slack, size: 36)
                BotMark(kind: .telegram, size: 36)
            }
            Text("No bots yet").font(.headline).foregroundStyle(Theme.text)
            Text("A bot lets the people on its list drive sessions from Slack or Telegram: a DM or a mention starts or continues one. Each bot runs on one of your machines.")
                .font(.subheadline).foregroundStyle(Theme.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 440)
            PillButton(title: "Add Bot", symbol: "plus", prominent: true, action: add)
                .disabled(!canAdd)
                .padding(.top, 4)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 48)
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

/// A chat's mark: its initial on a neutral tile.
struct BotMark: View {
    let kind: BotKind
    var size: CGFloat = 28

    var body: some View {
        Text(kind.label.prefix(1))
            .font(.system(size: size * 0.5, weight: .bold, design: .rounded))
            .foregroundStyle(Theme.secondary)
            .frame(width: size, height: size)
            .background(Theme.raised, in: .rect(cornerRadius: size * 0.28))
            .overlay(RoundedRectangle(cornerRadius: size * 0.28).strokeBorder(Theme.stroke))
            .accessibilityHidden(true)
    }
}

extension Fleet {
    /// A project's name, as its group lists it, or its id.
    func projectName(_ projectId: ProjectId) -> String {
        lists.projects.first { $0.projectId == projectId }?.name ?? projectId
    }
}

/// Adds a bot in four short steps: where it runs and its kind, its name, the chat's app to
/// create, then its tokens.
struct AddBotSheet: View {
    let fleet: Fleet
    @State private var setup: BotSetup
    @Environment(\.dismiss) private var dismiss

    init(fleet: Fleet) {
        self.fleet = fleet
        _setup = State(initialValue: BotSetup(fleet: fleet))
    }

    var body: some View {
        SheetScaffold(title: "Add Bot", height: 600, hint: "Tokens stay in the vault.") {
            AddBotForm(setup: setup)
        } footer: {
            Spacer()
            ActionButton(title: setup.actionTitle, style: .primary) {
                if await setup.save() { dismiss() }
            }
            .frame(width: 220)
            .disabled(setup.sending || setup.hostId == nil)
            .keyboardShortcut(.defaultAction)
        }
        .task { await setup.check(BotTarget.all(fleet.machines)) }
    }
}

/// The add-bot steps, on a `BotSetup` alone so they can be drawn without a fleet.
struct AddBotForm: View {
    @Bindable var setup: BotSetup
    @State private var showsManifest = false
    @Environment(\.openURL) private var openURL

    var body: some View {
        NumberedStep(number: 1, title: "Machine and kind") {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 10) { machinePicker; kindPicker }
                VStack(alignment: .leading, spacing: 10) { machinePicker; kindPicker }
            }
            if setup.hostId == nil, let problem = setup.targets.first?.problem {
                Text(setup.targets.count == 1 ? problem : "No machine can run a bot now. \(setup.targets[0].name): \(problem)")
                    .font(.footnote).foregroundStyle(Theme.secondary)
            }
        }
        NumberedStep(number: 2, title: "Name") {
            InputBox(placeholder: setup.machineName ?? "trash-can", text: $setup.name).accessibilityIdentifier("bot-name")
        }
        if setup.kind == .slack {
            NumberedStep(number: 3, title: "Create the Slack app") {
                Text("Create an app from this manifest at api.slack.com, then install it to your workspace.")
                    .font(.subheadline).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    CopyPill(title: "Copy manifest", text: BotSetup.slackManifest(name: setup.botName), prominent: true)
                    PillButton(title: "Open api.slack.com", symbol: "arrow.up.right") {
                        BotSetup.slackAppsURL.map { openURL($0) }
                    }
                }
                DisclosureGroup(isExpanded: $showsManifest) {
                    ManifestBox(text: BotSetup.slackManifest(name: setup.botName)).padding(.top, 6)
                } label: {
                    Text("Show manifest").font(.subheadline).foregroundStyle(Theme.secondary)
                }
                .tint(Theme.secondary)
            }
            NumberedStep(number: 4, title: "Tokens") {
                TokenField(label: "Bot token", hint: "OAuth & Permissions › Bot User OAuth Token",
                           placeholder: "xoxb-…", text: $setup.botToken.text)
                    .accessibilityIdentifier("bot-token")
                TokenField(label: "App-level token", hint: "Basic Information › App-Level Tokens, with connections:write",
                           placeholder: "xapp-…", text: $setup.appToken.text)
                    .accessibilityIdentifier("bot-app-token")
            }
        } else {
            NumberedStep(number: 3, title: "Create the Telegram bot") {
                Text("Message @BotFather, send /newbot and follow its questions.")
                    .font(.subheadline).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                PillButton(title: "Open @BotFather", symbol: "arrow.up.right") { BotSetup.botFatherURL.map { openURL($0) } }
            }
            NumberedStep(number: 4, title: "Token") {
                TokenField(label: "Bot token", hint: "The token @BotFather sends once the bot is made",
                           placeholder: "123456:ABC…", text: $setup.botToken.text)
                    .accessibilityIdentifier("bot-token")
            }
        }
        if let error = setup.error {
            Text(error).font(.footnote).foregroundStyle(Theme.failure).accessibilityIdentifier("bot-error")
        }
    }

    private var machinePicker: some View {
        Menu {
            ForEach(setup.targets) { target in
                Button { setup.hostId = target.hostId } label: {
                    Text(target.name)
                    if let problem = target.problem { Text(problem) }
                }
                .disabled(target.problem != nil)
            }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "desktopcomputer").foregroundStyle(Theme.tertiary)
                Text(setup.targets.first { $0.hostId == setup.hostId }?.name ?? (setup.targets.isEmpty ? "Checking machines…" : "No machine"))
                    .foregroundStyle(setup.hostId == nil ? Theme.tertiary : Theme.text)
                    .lineLimit(1)
                Spacer(minLength: 6)
                Image(systemName: "chevron.up.chevron.down").imageScale(.small).foregroundStyle(Theme.tertiary)
            }
            .font(.subheadline)
            .padding(.horizontal, 12)
            .frame(minWidth: 200, minHeight: 36)
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
            .contentShape(.rect)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .accessibilityIdentifier("bot-machine")
    }

    /// Slack or Telegram, as two segments in the sheet's dark style.
    private var kindPicker: some View {
        HStack(spacing: 2) {
            ForEach([BotKind.slack, .telegram], id: \.self) { kind in
                let selected = setup.kind == kind
                Button { setup.kind = kind } label: {
                    Text(kind.label)
                        .font(.subheadline.weight(selected ? .semibold : .regular))
                        .foregroundStyle(selected ? Theme.text : Theme.secondary)
                        .frame(maxWidth: .infinity, minHeight: 30)
                        .primaryFill(in: .rect(cornerRadius: Theme.corner - 2), on: selected, off: .clear)
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(selected ? .isSelected : [])
            }
        }
        .padding(2)
        .frame(width: 190)
        .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// A token's field: its name and where the chat shows it, then the hidden input.
private struct TokenField: View {
    let label: String
    let hint: String
    let placeholder: String
    @Binding var text: String

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 6) { title; place.lineLimit(1) }
                VStack(alignment: .leading, spacing: 2) { title; place.fixedSize(horizontal: false, vertical: true) }
            }
            TokenBox(placeholder: placeholder, text: $text)
        }
    }

    private var title: some View {
        Text(label).font(.footnote.weight(.semibold)).foregroundStyle(Theme.secondary)
    }

    private var place: Text {
        Text(hint).font(.footnote).foregroundColor(Theme.tertiary)
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
            .padding(.horizontal, 12)
            .frame(minHeight: 36)
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }
}

/// The app manifest, selectable.
private struct ManifestBox: View {
    let text: String

    var body: some View {
        ScrollView {
            Text(text)
                .font(Theme.monoSmall)
                .foregroundStyle(Theme.secondary)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(12)
        }
        .frame(maxHeight: 200)
        .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
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
            if let note = BotsNote(fleet: fleet, machine: machine, bots: bots) {
                note.padding(.horizontal, 6).padding(.vertical, 4)
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
