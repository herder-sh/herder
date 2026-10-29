import Herder
import SwiftUI

/// A bot's page, for its machine's owners, in the settings pattern: a row per person with their
/// role, what it knows (its instructions and memory), one row summing up what new threads run
/// on, the channels with a project of their own, then removing it. Each row opens its details in
/// a sheet; each change applies as it is made.
struct BotPage: View {
    let fleet: Fleet
    let hostId: HostId
    let botId: BotId
    @State private var bots: MachineBots
    @State private var knowledge: BotKnowledge
    /// The machine's people, whom a bot's person can be linked to.
    @State private var users: People
    @State private var saves = Autosave()
    @State private var sheet: BotSheet?
    @State private var confirmingRemove = false
    @State private var error: String?
    @Environment(\.pages) private var pages

    init(fleet: Fleet, hostId: HostId, botId: BotId) {
        self.init(fleet: fleet, bots: MachineBots(fleet: fleet, hostId: hostId), botId: botId)
    }

    init(fleet: Fleet, bots: MachineBots, botId: BotId) {
        self.fleet = fleet
        self.hostId = bots.hostId
        self.botId = botId
        let owner = fleet.machines.first { $0.hostId == bots.hostId }?.role == .owner
        _bots = State(initialValue: bots)
        _knowledge = State(initialValue: BotKnowledge(bots: bots, botId: botId, editable: owner))
        _users = State(initialValue: People(fleet: fleet, hostId: bots.hostId))
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
                    BotKnowledgeRow(knowledge: knowledge) { sheet = .knowledge }
                        .detailSection("Instructions & memory")
                    newThreads(bot).detailSection("New threads")
                    channels(bot).detailSection("Channels")
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
            await knowledge.reload()
            await users.reload()
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(5))
                await bots.reloadStatus()
            }
        }
        .sheet(item: $sheet) { sheet in
            if let bot {
                switch sheet {
                case .person(let id): personSheet(bot, id: id)
                case .addPerson: AddPersonSheet(bot: bot) { user in change(user.externalUserId) { $0.allow.append(user) } }
                case .knowledge: BotKnowledgeSheet(knowledge: knowledge, name: bot.name)
                case .newThreads: newThreadsSheet(bot, machine: machine)
                case .channel(let id): channelSheet(bot, id: id, machine: machine)
                case .addChannel: AddChannelSheet(bot: bot, projects: machine?.projects ?? [], projectName: fleet.projectName) { binding in
                    change(binding.channelId) { $0.channelBindings.append(binding) }
                }
                }
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
        SettingsList(title: "People", caption: BotRole.legend) {
            if bot.allow.isEmpty {
                EmptyRow(text: "No one yet: the bot answers no one until you add them.")
                RowDivider()
            }
            if let hint = BotRole.hint(bot.allow) {
                EmptyRow(text: hint).accessibilityIdentifier("bot-no-owner")
                RowDivider()
            }
            ForEach(bot.allow, id: \.externalUserId) { user in
                personRow(user)
                RowDivider()
            }
            AddRow(title: "Add person…") { sheet = .addPerson }
                .accessibilityIdentifier("bot-add-person")
        }
    }

    private func personRow(_ user: BotUser) -> some View {
        let id = user.externalUserId
        let role = BotRole(user)
        return SummaryRow(title: user.displayName, summary: role == .custom ? user.abilities : "", saved: saves[id],
                          open: { sheet = .person(id) }) {
            Avatar(name: user.displayName, id: id)
        } accessory: {
            Menu {
                ForEach(BotRole.presets) { preset in
                    Button {
                        change(id) { bot in
                            if let index = bot.allow.firstIndex(where: { $0.externalUserId == id }) {
                                bot.allow[index] = preset.applied(to: bot.allow[index])
                            }
                        }
                    } label: {
                        if preset == role { Label(preset.label, systemImage: "checkmark") } else { Text(preset.label) }
                    }
                }
                Divider()
                Button("Custom…") { sheet = .person(id) }
            } label: {
                HStack(spacing: 4) {
                    Text(role.label)
                    Image(systemName: "chevron.down").font(.caption2.weight(.semibold))
                }
                .font(.caption.weight(.medium))
                .foregroundStyle(Theme.text)
                .padding(.horizontal, 10)
                .frame(height: 26)
                .background(Theme.raised, in: .rect(cornerRadius: 7))
                .overlay(RoundedRectangle(cornerRadius: 7).strokeBorder(Theme.stroke))
                .hitTarget()
            }
            .menuStyle(.button)
            .buttonStyle(.plain)
            .menuIndicator(.hidden)
            .fixedSize()
            .accessibilityLabel("Role of \(user.displayName)")
            .accessibilityValue(role.label)
        }
        .accessibilityIdentifier("bot-person-\(id)")
    }

    @ViewBuilder private func personSheet(_ bot: Bot, id: String) -> some View {
        if let user = bot.allow.first(where: { $0.externalUserId == id }) {
            let paired: [PersonInfo] = if case .loaded(let loaded) = users.people { loaded } else { [] }
            BotPersonSheet(bot: bot.name, user: user, users: paired, saved: saves[id]) { next in
                change(id) { bot in
                    if let index = bot.allow.firstIndex(where: { $0.externalUserId == id }) { bot.allow[index] = next }
                }
            } remove: {
                sheet = nil
                change(id) { $0.allow.removeAll { $0.externalUserId == id } }
            }
        }
    }

    // MARK: New threads

    private func newThreads(_ bot: Bot) -> some View {
        SettingsList(title: "New threads") {
            SummaryRow(title: Self.newThreads(bot.defaults, projectName: fleet.projectName),
                       summary: "Project, provider, accounts, model and permissions",
                       saved: Self.defaultsRows.lazy.compactMap { saves[$0] }.first, open: { sheet = .newThreads })
                .accessibilityIdentifier("bot-new-threads")
        }
    }

    static let defaultsRows = ["Project", "Provider", "Accounts", "Model", "Permissions"]

    /// "api · Claude Opus 5.5 · Full access": what a new thread runs on, each part left out when
    /// new sessions pick it.
    static func newThreads(_ defaults: BotDefaults, projectName: (ProjectId) -> String) -> String {
        let model = defaults.model.map { id in
            defaults.provider.flatMap { ModelCatalog.models($0).first { $0.id == id }?.name } ?? id
        } ?? defaults.provider.map(ModelCatalog.providerName)
        let parts = [defaults.projectId.map(projectName) ?? "A chat", model, defaults.permissionMode?.label].compactMap(\.self)
        return parts.joined(separator: " · ")
    }

    func newThreadsSheet(_ bot: Bot, machine: Machine?) -> some View {
        let accounts = machine?.accounts ?? []
        let providers = Array(Set(accounts.map(\.provider))).sorted()
        let provider = bot.defaults.provider
        let ofProvider = accounts.filter { $0.provider == provider }
        return DetailSheet(title: "New threads") {
            SettingsGroup {
                SettingRow(label: "Project", hint: "Used when the channel has no project", saved: saves["Project"]) {
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
                    FooterItem(symbol: "sparkles", text: bot.defaults.model.flatMap { id in
                        provider.flatMap { ModelCatalog.models($0).first { $0.id == id }?.name } ?? id
                    } ?? "Provider’s default") {
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
            .foregroundStyle(Theme.action)
        }
    }

    /// "Any" with none picked, else the picked ones' labels.
    static func accounts(_ ids: [AccountId], of accounts: [Account]) -> String {
        if ids.isEmpty { return "Any" }
        return ids.map { id in accounts.first { $0.accountId == id }?.label ?? id }.joined(separator: ", ")
    }

    // MARK: Channels

    private func channels(_ bot: Bot) -> some View {
        SettingsList(title: "Channels", caption: "`@\(bot.name) use ‹project›` in a channel gives it its own project too.") {
            if bot.channelBindings.isEmpty {
                EmptyRow(text: "Every channel uses the default")
            }
            ForEach(Array(bot.channelBindings.enumerated()), id: \.element.channelId) { index, binding in
                if index > 0 { RowDivider() }
                SummaryRow(title: binding.channelId, value: fleet.projectName(binding.projectId), saved: saves[binding.channelId],
                           open: { sheet = .channel(binding.channelId) })
            }
            RowDivider()
            AddRow(title: "Give a channel its own project…") { sheet = .addChannel }
                .accessibilityIdentifier("bot-add-channel")
        }
    }

    @ViewBuilder private func channelSheet(_ bot: Bot, id: String, machine: Machine?) -> some View {
        if let binding = bot.channelBindings.first(where: { $0.channelId == id }) {
            DetailSheet(title: "Channel", id: id, destructive: "Use the default", onDestructive: {
                sheet = nil
                change(id) { $0.channelBindings.removeAll { $0.channelId == id } }
            }) {
                SettingsGroup {
                    SettingRow(label: "Project", hint: "New threads here work on it", saved: saves[id]) {
                        FooterItem(symbol: "shippingbox", text: fleet.projectName(binding.projectId)) {
                            Picker("Project", selection: Binding(get: { binding.projectId }, set: { project in
                                change(id) { bot in
                                    if let index = bot.channelBindings.firstIndex(where: { $0.channelId == id }) {
                                        bot.channelBindings[index].projectId = project
                                    }
                                }
                            })) {
                                ForEach(machine?.projects ?? [], id: \.projectId) { Text($0.name).tag($0.projectId) }
                            }
                            .pickerStyle(.inline)
                        }
                        .font(.subheadline)
                        .foregroundStyle(Theme.action)
                    }
                }
            }
        }
    }
}

/// The sheet a bot's page has open: a person's, adding one, its instructions and memory, new
/// threads', a channel's, or giving a channel a project.
enum BotSheet: Identifiable, Hashable {
    case person(String)
    case addPerson
    case knowledge
    case newThreads
    case channel(String)
    case addChannel

    var id: Self { self }
}

/// A person's details: their member ID, the five switches their role stands for, each with
/// what it lets them do, and the herder user they are; removing them bottom left.
struct BotPersonSheet: View {
    /// The bot's name.
    let bot: String
    let user: BotUser
    /// The machine's people, whom they can be linked to.
    var users: [PersonInfo] = []
    var saved: SaveState?
    let set: (BotUser) -> Void
    let remove: () -> Void

    var body: some View {
        let name = user.displayName
        DetailSheet(title: name, id: user.externalUserId, destructive: "Remove \(name)", onDestructive: remove) {
            SettingsGroup {
                SwitchRow(title: "Message the bot directly", detail: "\(name)’s DMs to \(bot) start sessions",
                          isOn: binding(\.dm))
                RowDivider()
                SwitchRow(title: "Mention it in channels", detail: "@\(bot) in a channel starts a thread",
                          isOn: binding(\.channels))
                RowDivider()
                SwitchRow(title: "Approve requests", detail: "May press Allow when an agent asks",
                          isOn: binding(\.approve))
                RowDivider()
                SwitchRow(title: "Get notified", detail: "A DM when a session waits for them or fails",
                          isOn: binding(\.notify))
                RowDivider()
                SwitchRow(title: "Manage the bot", detail: "May bind channels to projects and change \(bot)’s memory from chat",
                          isOn: binding(\.owner))
            }
            SettingsGroup {
                SettingRow(label: "herder user", hint: "Agents run herder commands for \(name)’s prompts with this user’s role") {
                    FooterItem(symbol: "person.crop.circle", text: linked) {
                        Picker("herder user", selection: Binding(get: { user.userId }, set: { userId in
                            var next = user
                            next.userId = userId
                            set(next)
                        })) {
                            Text("None").tag(UserId?.none)
                            ForEach(users, id: \.personId) { Text($0.name).tag(Optional($0.personId)) }
                        }
                        .pickerStyle(.inline)
                    }
                }
                .accessibilityIdentifier("bot-person-user")
            }
            HStack(spacing: 6) {
                Text("Role: \(BotRole(user).label)").foregroundStyle(Theme.secondary)
                SaveMark(state: saved)
                if case .failed(let why) = saved { Text(why).foregroundStyle(Theme.failure) }
            }
            .font(.footnote)
        }
    }

    /// The user they are linked to, by name; their id when the machine no longer has them.
    private var linked: String {
        guard let userId = user.userId else { return "None" }
        return users.first { $0.personId == userId }?.name ?? userId
    }

    private func binding(_ key: WritableKeyPath<BotUser, Bool>) -> Binding<Bool> {
        Binding(get: { user[keyPath: key] }, set: { on in
            var next = user
            next[keyPath: key] = on
            set(next)
        })
    }
}

/// Adds a person to a bot: their member ID, which the chat does not list for the bot, and name.
/// They start as an approver.
private struct AddPersonSheet: View {
    let bot: Bot
    let add: (BotUser) -> Void
    @State private var person = BotPersonDraft()
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        DetailSheet(title: "Add person", primary: "Add", onPrimary: save,
                    primaryDisabled: person.memberId.trimmingCharacters(in: .whitespaces).isEmpty
                        || person.name.trimmingCharacters(in: .whitespaces).isEmpty) {
            Field(label: "Member ID", hint: "In Slack, open their profile, then ⋯ › Copy member ID.") {
                InputBox(placeholder: "U012AB3CD", text: $person.memberId, mono: true)
                    .accessibilityIdentifier("bot-person-id")
            }
            Field(label: "Name", hint: "How herder names their actions: “via \(bot.name), Sam”.") {
                InputBox(placeholder: "Sam", text: $person.name)
                    .accessibilityIdentifier("bot-person-name")
            }
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
            }
        }
    }

    private func save() {
        do {
            guard let user = try person.person(among: bot.allow) else { return }
            add(user)
            dismiss()
        } catch {
            self.error = describe(error)
        }
    }
}

/// Gives a channel a project of its own: its ID and the project.
private struct AddChannelSheet: View {
    let bot: Bot
    let projects: [Project]
    let projectName: (ProjectId) -> String
    let bind: (ChannelBinding) -> Void
    @State private var channel = ""
    @State private var project: ProjectId?
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        DetailSheet(title: "Give a channel its own project", primary: "Save", onPrimary: save,
                    primaryDisabled: channel.trimmingCharacters(in: .whitespaces).isEmpty || project == nil) {
            Field(label: "Channel ID", hint: "Slack shows it at the bottom of the channel’s details.") {
                InputBox(placeholder: "C012AB3CD", text: $channel, mono: true)
                    .accessibilityIdentifier("bot-channel-id")
            }
            Field(label: "Project") {
                FooterItem(symbol: "shippingbox", text: project.map(projectName) ?? "Pick a project") {
                    Picker("Project", selection: $project) {
                        ForEach(projects, id: \.projectId) { Text($0.name).tag(Optional($0.projectId)) }
                    }
                    .pickerStyle(.inline)
                }
                .font(.subheadline)
                .foregroundStyle(Theme.action)
            }
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
            }
        }
    }

    private func save() {
        let id = channel.trimmingCharacters(in: .whitespaces).uppercased()
        guard let project else { return }
        if bot.channelBindings.contains(where: { $0.channelId == id }) {
            error = "\(id) has a project already; change it in its row."
            return
        }
        bind(ChannelBinding(channelId: id, projectId: project))
        dismiss()
    }
}
