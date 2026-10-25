import Herder
import SwiftUI

/// A bot's page, for its machine's owners: the people it answers with what each may do, what
/// the sessions it starts run on, the project of each channel, then removing it. Each change
/// applies as it is made.
struct BotPage: View {
    let fleet: Fleet
    let hostId: HostId
    let botId: BotId
    @State private var bots: MachineBots
    @State private var saves = Autosave()
    @State private var person = BotPersonDraft()
    @State private var channel = ""
    @State private var channelProject: ProjectId?
    @State private var confirmingRemove = false
    @State private var error: String?
    @Environment(\.pages) private var pages

    init(fleet: Fleet, hostId: HostId, botId: BotId) {
        self.fleet = fleet
        self.hostId = hostId
        self.botId = botId
        _bots = State(initialValue: MachineBots(fleet: fleet, hostId: hostId))
    }

    var body: some View {
        let machine = fleet.machines.first { $0.hostId == hostId }
        let bot = bots.bot(botId)
        let status = bots.statuses[botId]
        DetailPage(title: bot?.name ?? botId, chip: status.map { StatusChip(text: $0.state.label, urgency: $0.state.urgency) },
                   status: bot.map { "\($0.kind.label) · \(machine?.name ?? hostId) · \(Self.people($0.allow.count))" } ?? "",
                   leading: { BotMark(kind: bot?.kind ?? .slack, size: 36) }) {
            IconButton(symbol: "arrow.clockwise", help: "Reload") { Task { await bots.reload() } }
        } content: {
            switch bots.bots {
            case .asking:
                ProgressView().frame(maxWidth: .infinity)
            case .failed(let why):
                Text(why).font(.footnote).foregroundStyle(Theme.failure)
            case .loaded:
                if let bot {
                    if let status, let fix = status.state.fix {
                        NeedsAttention {
                            AttentionRow(title: status.state.label, detail: status.lastError?.message ?? fix,
                                         urgency: status.state.urgency) { EmptyView() }
                        }
                    }
                    people(bot).detailSection("People")
                    defaults(bot, machine: machine).detailSection("Defaults")
                    channels(bot, machine: machine).detailSection("Channels")
                    if let error {
                        Text(error).font(.footnote).foregroundStyle(Theme.failure).accessibilityIdentifier("bot-error")
                    }
                    AboutSection {
                        DetailRow(label: "Bot id", value: bot.botId, mono: true)
                        DetailRow(label: "Machine", value: machine?.name ?? hostId)
                        DetailRow(label: "Bot token", value: bot.token, mono: true)
                        if let app = bot.appToken { DetailRow(label: "App token", value: app, mono: true) }
                        if let last = status?.lastError {
                            DetailRow(label: "Last error", value: last.message)
                        }
                    }
                    DangerZone {
                        SettingRow(label: "Remove this bot", hint: "Its tokens stay in the vault; linked sessions keep their threads") {
                            DangerButton(title: "Remove…") { confirmingRemove = true }
                        }
                    }
                } else {
                    Text("The machine no longer runs this bot.").foregroundStyle(Theme.secondary)
                }
            }
        }
        .task {
            await bots.reload()
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(5))
                await bots.reloadStatus()
            }
        }
        .confirmationDialog("Remove \(bot?.name ?? botId)?", isPresented: $confirmingRemove, titleVisibility: .visible) {
            Button("Remove", role: .destructive) {
                Task {
                    do {
                        try await bots.remove(botId)
                        pages.close()
                    } catch {
                        self.error = describe(error)
                    }
                }
            }
        } message: {
            Text("It disconnects from its chat now.")
        }
    }

    static func people(_ count: Int) -> String {
        count == 1 ? "1 person" : "\(count) people"
    }

    /// Applies `change` to the bot, showing how it went beside `row`.
    private func change(_ row: String, _ change: @escaping (inout Bot) -> Void) {
        Task { await saves.save(row) { try await bots.update(botId, change) } }
    }

    // MARK: People

    private func people(_ bot: Bot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            SettingsList(title: "People") {
                if bot.allow.isEmpty {
                    Text("No one yet: the bot answers no one until you add them.")
                        .font(.subheadline).foregroundStyle(Theme.tertiary)
                        .padding(.horizontal, 14).frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                }
                ForEach(Array(bot.allow.enumerated()), id: \.element.externalUserId) { index, user in
                    if index > 0 { RowDivider() }
                    personRow(user)
                }
                RowDivider()
                HStack(spacing: 10) {
                    InputBox(placeholder: "Member ID, U012AB3CD", text: $person.memberId, mono: true)
                        .accessibilityIdentifier("bot-person-id")
                    InputBox(placeholder: "Name", text: $person.name)
                        .accessibilityIdentifier("bot-person-name")
                    PillButton(title: "Add", symbol: "plus", prominent: true) { addPerson(bot) }
                        .disabled(person.memberId.trimmingCharacters(in: .whitespaces).isEmpty
                                  || person.name.trimmingCharacters(in: .whitespaces).isEmpty)
                }
                .padding(10)
            }
            Text("Slack shows a member ID in their profile, under ⋯ › Copy member ID. Only people here drive sessions or press Allow; others get a note.")
                .font(.footnote).foregroundStyle(Theme.tertiary)
        }
    }

    private func personRow(_ user: BotUser) -> some View {
        let id = user.externalUserId
        return VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(user.displayName).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text).lineLimit(1)
                    Text(id).font(Theme.monoSmall).foregroundStyle(Theme.tertiary)
                }
                Spacer(minLength: 8)
                SaveMark(state: saves[id])
                IconButton(symbol: "minus.circle", help: "Remove \(user.displayName)") {
                    change(id) { $0.allow.removeAll { $0.externalUserId == id } }
                }
            }
            HStack(spacing: 14) {
                permission("DM", user.dm, id) { $0.dm = $1 }
                permission("Channels", user.channels, id) { $0.channels = $1 }
                permission("Approve", user.approve, id) { $0.approve = $1 }
                permission("Notify", user.notify, id) { $0.notify = $1 }
            }
            if case .failed(let why) = saves[id] {
                Text(why).font(.caption).foregroundStyle(Theme.failure)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    private func permission(_ label: String, _ on: Bool, _ id: String,
                            _ set: @escaping (inout BotUser, Bool) -> Void) -> some View {
        Toggle(label, isOn: Binding(get: { on }, set: { value in
            change(id) { bot in
                if let index = bot.allow.firstIndex(where: { $0.externalUserId == id }) { set(&bot.allow[index], value) }
            }
        }))
        .toggleStyle(.switch)
        .controlSize(.mini)
        .font(.caption)
        .foregroundStyle(Theme.secondary)
        .fixedSize()
    }

    private func addPerson(_ bot: Bot) {
        do {
            guard let user = try person.person(among: bot.allow) else { return }
            person = BotPersonDraft()
            error = nil
            change(user.externalUserId) { $0.allow.append(user) }
        } catch {
            self.error = describe(error)
        }
    }

    // MARK: Defaults

    private func defaults(_ bot: Bot, machine: Machine?) -> some View {
        let accounts = machine?.accounts ?? []
        let providers = Array(Set(accounts.map(\.provider))).sorted()
        let provider = bot.defaults.provider
        let ofProvider = accounts.filter { $0.provider == provider }
        return SettingsList(title: "Defaults") {
            SettingRow(label: "Project", hint: "Of a new thread whose channel and message name none", saved: saves["Project"]) {
                FooterItem(symbol: "shippingbox", text: bot.defaults.projectId.map { fleet.projectName($0) } ?? "None: a chat") {
                    Picker("Project", selection: Binding(get: { bot.defaults.projectId }, set: { project in
                        change("Project") { $0.defaults.projectId = project }
                    })) {
                        Text("None: a chat").tag(ProjectId?.none)
                        ForEach(machine?.projects ?? [], id: \.projectId) { Text($0.name).tag(Optional($0.projectId)) }
                    }
                    .pickerStyle(.inline)
                }
            }
            RowDivider()
            SettingRow(label: "Provider", saved: saves["Provider"]) {
                FooterItem(symbol: "cpu", text: provider.map(ModelCatalog.providerName) ?? "As new sessions pick") {
                    Picker("Provider", selection: Binding(get: { provider }, set: { next in
                        change("Provider") {
                            $0.defaults.provider = next
                            $0.defaults.accountIds = []
                            $0.defaults.model = nil
                        }
                    })) {
                        Text("As new sessions pick").tag(Provider?.none)
                        ForEach(providers, id: \.self) { Text(ModelCatalog.providerName($0)).tag(Optional($0)) }
                    }
                    .pickerStyle(.inline)
                }
            }
            RowDivider()
            SettingRow(label: "Accounts", hint: "Sessions start on and rotate across these", saved: saves["Accounts"]) {
                FooterItem(symbol: "person.crop.circle", text: Self.accounts(bot.defaults.accountIds, of: ofProvider)) {
                    ForEach(ofProvider, id: \.accountId) { account in
                        Toggle(account.email ?? account.label, isOn: Binding(
                            get: { bot.defaults.accountIds.contains(account.accountId) },
                            set: { on in
                                change("Accounts") { bot in
                                    bot.defaults.accountIds.removeAll { $0 == account.accountId }
                                    if on { bot.defaults.accountIds.append(account.accountId) }
                                }
                            }))
                    }
                }
                .disabled(provider == nil)
            }
            RowDivider()
            SettingRow(label: "Model", saved: saves["Model"]) {
                FooterItem(symbol: "sparkles", text: bot.defaults.model ?? "Provider’s default") {
                    Picker("Model", selection: Binding(get: { bot.defaults.model }, set: { model in
                        change("Model") { $0.defaults.model = model }
                    })) {
                        Text("Provider’s default").tag(String?.none)
                        ForEach(provider.map(ModelCatalog.models) ?? [], id: \.id) { Text($0.name).tag(Optional($0.id)) }
                    }
                    .pickerStyle(.inline)
                }
                .disabled(provider == nil)
            }
            RowDivider()
            SettingRow(label: "Permissions", hint: "What agents may do without asking", saved: saves["Permissions"]) {
                FooterItem(symbol: "lock.shield", text: bot.defaults.permissionMode?.label ?? "As new sessions pick") {
                    Picker("Permissions", selection: Binding(get: { bot.defaults.permissionMode }, set: { mode in
                        change("Permissions") { $0.defaults.permissionMode = mode }
                    })) {
                        Text("As new sessions pick").tag(PermissionMode?.none)
                        ForEach([PermissionMode.readOnly, .ask, .autoEdit, .fullAccess], id: \.self) {
                            Text($0.label).tag(Optional($0))
                        }
                    }
                    .pickerStyle(.inline)
                }
            }
        }
        .font(.subheadline)
        .foregroundStyle(Theme.secondary)
    }

    /// "Any" with none picked, else the picked ones' labels.
    static func accounts(_ ids: [AccountId], of accounts: [Account]) -> String {
        if ids.isEmpty { return "Any" }
        return ids.map { id in accounts.first { $0.accountId == id }?.label ?? id }.joined(separator: ", ")
    }

    // MARK: Channels

    private func channels(_ bot: Bot, machine: Machine?) -> some View {
        let projects = machine?.projects ?? []
        return VStack(alignment: .leading, spacing: 8) {
            SettingsList(title: "Channels") {
                if bot.channelBindings.isEmpty {
                    Text("No channel has a project of its own.")
                        .font(.subheadline).foregroundStyle(Theme.tertiary)
                        .padding(.horizontal, 14).frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                }
                ForEach(Array(bot.channelBindings.enumerated()), id: \.element.channelId) { index, binding in
                    if index > 0 { RowDivider() }
                    let id = binding.channelId
                    SettingRow(label: id, saved: saves[id]) {
                        FooterItem(symbol: "shippingbox", text: fleet.projectName(binding.projectId)) {
                            Picker("Project", selection: Binding(get: { binding.projectId }, set: { project in
                                change(id) { bot in
                                    if let index = bot.channelBindings.firstIndex(where: { $0.channelId == id }) {
                                        bot.channelBindings[index].projectId = project
                                    }
                                }
                            })) {
                                ForEach(projects, id: \.projectId) { Text($0.name).tag($0.projectId) }
                            }
                            .pickerStyle(.inline)
                        }
                        .font(.subheadline)
                        .foregroundStyle(Theme.secondary)
                        IconButton(symbol: "minus.circle", help: "Unbind \(id)") {
                            change(id) { $0.channelBindings.removeAll { $0.channelId == id } }
                        }
                    }
                }
                RowDivider()
                HStack(spacing: 10) {
                    InputBox(placeholder: "Channel ID, C012AB3CD", text: $channel, mono: true)
                        .accessibilityIdentifier("bot-channel-id")
                    FooterItem(symbol: "shippingbox", text: channelProject.map { fleet.projectName($0) } ?? "Project") {
                        Picker("Project", selection: $channelProject) {
                            ForEach(projects, id: \.projectId) { Text($0.name).tag(Optional($0.projectId)) }
                        }
                        .pickerStyle(.inline)
                    }
                    .font(.subheadline)
                    .foregroundStyle(Theme.secondary)
                    PillButton(title: "Bind", symbol: "plus", prominent: true) { bind(bot) }
                        .disabled(channel.trimmingCharacters(in: .whitespaces).isEmpty || channelProject == nil)
                }
                .padding(10)
            }
            Text("New threads in a bound channel work on its project. Slack shows a channel’s ID at the bottom of its details; `@bot use ‹project›` in the channel binds it too.")
                .font(.footnote).foregroundStyle(Theme.tertiary)
        }
    }

    private func bind(_ bot: Bot) {
        let id = channel.trimmingCharacters(in: .whitespaces).uppercased()
        guard let project = channelProject else { return }
        if bot.channelBindings.contains(where: { $0.channelId == id }) {
            error = "\(id) has a project already; pick another in its row."
            return
        }
        channel = ""
        channelProject = nil
        error = nil
        change(id) { $0.channelBindings.append(ChannelBinding(channelId: id, projectId: project)) }
    }
}
