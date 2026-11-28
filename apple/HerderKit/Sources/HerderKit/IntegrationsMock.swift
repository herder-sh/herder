import SwiftUI

// MOCK (P30.1) · DEMO ONLY.
//
// The Integrations catalog, an integration's page and the install sheet, drawn from the canned
// data below so the user can review the model in docs/integrations.md before any contract
// exists. Nothing here talks to a daemon: no protocol type, no client-core call, no secret is
// read or stored. The app shows it only inside the demo (`Fleet.isDemo`): the sidebar's row,
// the iPhone's Hub link and the pages all check it. Delete this file, and the `isDemo` checks
// that lead here, when the real feature lands.

/// Who stands behind an integration, which sets how much the catalog vouches for it.
enum IntegrationTrust: Hashable {
    /// Ships inside herder, reviewed with it.
    case builtIn
    /// From a publisher herder has checked: the vendor itself, signed.
    case verified
    /// Anyone's, listed as is.
    case community

    var label: String {
        switch self {
        case .builtIn: "Built in"
        case .verified: "Verified"
        case .community: "Community"
        }
    }

    var symbol: String {
        switch self {
        case .builtIn: "checkmark.seal.fill"
        case .verified: "checkmark.seal"
        case .community: "person.2"
        }
    }
}

/// What an integration adds to herder; one integration may add several.
enum IntegrationCapability: Hashable {
    /// People talk to sessions from a chat, as Slack and Telegram bots do today.
    case chat
    /// Tools agents call, served over MCP.
    case tools
    /// Events from the service start or wake sessions: an issue assigned, a check failed.
    case triggers
    /// Skills the integration ships, added to the library.
    case skills

    var title: String {
        switch self {
        case .chat: "Chat bridge"
        case .tools: "Tools"
        case .triggers: "Triggers"
        case .skills: "Skills"
        }
    }

    var symbol: String {
        switch self {
        case .chat: "bubble.left.and.bubble.right"
        case .tools: "wrench.and.screwdriver"
        case .triggers: "bolt"
        case .skills: "book.closed"
        }
    }
}

/// Where an installed integration runs.
enum IntegrationPlace: Hashable {
    /// On one machine you own, as a bot does today.
    case machine
    /// On the hub, the one place that is always on, for webhooks.
    case hub
    /// On every machine whose sessions use it, as an MCP server does.
    case sessions

    var label: String {
        switch self {
        case .machine: "On one machine"
        case .hub: "On the hub"
        case .sessions: "With each session"
        }
    }
}

/// The catalog's filter.
enum IntegrationCategory: String, CaseIterable, Hashable {
    case all = "All", chat = "Chat", code = "Code", issues = "Issues", monitoring = "Monitoring", tools = "Tools"
}

/// How an installed integration is doing.
enum IntegrationHealth: Hashable {
    case healthy, needsYou, failing, off

    var chip: String {
        switch self {
        case .healthy: "Healthy"
        case .needsYou: "Needs setup"
        case .failing: "Failing"
        case .off: "Off"
        }
    }

    var urgency: Urgency {
        switch self {
        case .healthy: .done
        case .needsYou: .needsYou
        case .failing: .failing
        case .off: .quiet
        }
    }
}

/// One line of an installed integration's log.
struct IntegrationLogLine: Hashable {
    let when: String
    let text: String
    var failed = false
}

/// What an installed integration has: where it runs and what it may touch.
struct IntegrationInstall: Hashable {
    var health: IntegrationHealth
    /// The machines it runs on, or reaches for sessions.
    var machines: [String]
    /// The projects it may touch; empty for every project.
    var projects: [String]
    /// One line of what it last did.
    var lastEvent: String
    /// What is wrong, with its fix's title, when it is not healthy.
    var problem: (title: String, fix: String)?
    var log: [IntegrationLogLine]

    static func == (left: Self, right: Self) -> Bool {
        left.health == right.health && left.machines == right.machines && left.projects == right.projects
            && left.lastEvent == right.lastEvent && left.problem?.title == right.problem?.title && left.log == right.log
    }

    func hash(into hasher: inout Hasher) {
        hasher.combine(machines)
        hasher.combine(lastEvent)
    }
}

/// An integration, installed or in the catalog: the shape docs/integrations.md proposes.
struct MockIntegration: Identifiable, Hashable {
    /// Reverse-DNS, unique in the catalog: `sh.herder.slack`.
    let id: String
    let name: String
    let publisher: String
    let trust: IntegrationTrust
    let version: String
    let category: IntegrationCategory
    let summary: String
    let provides: [IntegrationCapability]
    let place: IntegrationPlace
    /// What it may do in the service and in herder, as the install sheet asks for it.
    let scopes: [String]
    /// The secrets it needs, by the name they are kept under on the hub.
    let secrets: [String]
    /// Its settings, each a switch, as its page shows them.
    let settings: [(title: String, detail: String, on: Bool)]
    let symbol: String
    let tint: Color
    var installed: IntegrationInstall?

    static func == (left: Self, right: Self) -> Bool { left.id == right.id && left.installed == right.installed }
    func hash(into hasher: inout Hasher) { hasher.combine(id) }

    /// "Chat bridge · Tools".
    var providesLine: String { provides.map(\.title).joined(separator: " · ") }
}

/// The demo catalog. Every name, id and line in it is made up for the mock.
enum MockIntegrations {
    static func find(_ id: String) -> MockIntegration? { catalog.first { $0.id == id } }

    static var installed: [MockIntegration] { catalog.filter { $0.installed != nil } }
    static var available: [MockIntegration] { catalog.filter { $0.installed == nil } }

    /// "4 installed · 1 needs you · 1 failing · 9 available".
    static var status: String {
        let needing = installed.filter { $0.installed?.health == .needsYou }.count
        let failing = installed.filter { $0.installed?.health == .failing }.count
        return (["\(installed.count) installed"]
            + (needing > 0 ? ["\(needing) needs you"] : [])
            + (failing > 0 ? ["\(failing) failing"] : [])
            + ["\(available.count) available"]).joined(separator: " · ")
    }

    static let catalog: [MockIntegration] = [
        MockIntegration(
            id: "sh.herder.slack", name: "Slack", publisher: "herder", trust: .builtIn, version: "1.4.0", category: .chat,
            summary: "Talk to sessions from Slack: DMs, channels and threads, approvals with a tap.",
            provides: [.chat], place: .machine,
            scopes: ["Read and write messages where it is added", "Start sessions in the projects you pick",
                     "Ask the people you allow to approve"],
            secrets: ["SLACK_BOT_TOKEN", "SLACK_APP_TOKEN"],
            settings: [("Answer in threads", "Each conversation stays in its own thread.", true),
                       ("Approvals in Slack", "People allowed to approve can do it from the message.", true),
                       ("Notify when a session finishes", "Posts a line in the thread it started from.", false)],
            symbol: "number", tint: Theme.avatars[0],
            installed: IntegrationInstall(
                health: .healthy, machines: ["studio"], projects: ["api", "web"],
                lastEvent: "Started a session in #web 4 min. ago", problem: nil,
                log: [IntegrationLogLine(when: "4 min. ago", text: "Sam started a session in #web"),
                      IntegrationLogLine(when: "1 hr. ago", text: "Kim approved Run pnpm e2e"),
                      IntegrationLogLine(when: "yesterday", text: "Connected over Socket Mode")])),
        MockIntegration(
            id: "sh.herder.telegram", name: "Telegram", publisher: "herder", trust: .builtIn, version: "1.4.0", category: .chat,
            summary: "A Telegram bot for your sessions, for the phone in your pocket.",
            provides: [.chat], place: .machine,
            scopes: ["Read and write messages to the bot", "Start chats, not project sessions, unless you pick projects"],
            secrets: ["TELEGRAM_BOT_TOKEN"],
            settings: [("Approvals in Telegram", "People allowed to approve can do it from the message.", false)],
            symbol: "paperplane", tint: Theme.avatars[3],
            installed: IntegrationInstall(
                health: .needsYou, machines: ["laptop"], projects: [],
                lastEvent: "Waiting for its first message", problem: ("Nobody is allowed to talk to it yet", "Add Person"),
                log: [IntegrationLogLine(when: "2 days ago", text: "Installed on laptop")])),
        MockIntegration(
            id: "sh.herder.github", name: "GitHub", publisher: "herder", trust: .builtIn, version: "0.9.2", category: .code,
            summary: "Start a session from an issue or a failed check; agents read and comment on pull requests.",
            provides: [.triggers, .tools], place: .hub,
            scopes: ["Issues and pull requests: read and write", "Checks: read", "Webhooks to the hub",
                     "Only the repositories of the projects you pick"],
            secrets: ["GITHUB_APP_KEY"],
            settings: [("Start a session when an issue is labelled herder", "In the issue's project, on its default machine.", true),
                       ("Fix failing checks", "A failed check on a herder branch wakes its session.", true),
                       ("Comment the session's link on the PR", "", false)],
            symbol: "chevron.left.forwardslash.chevron.right", tint: Theme.avatars[4],
            installed: IntegrationInstall(
                health: .healthy, machines: ["hub"], projects: ["api", "web", "site"],
                lastEvent: "Woke Fix flaky checkout test 12 min. ago", problem: nil,
                log: [IntegrationLogLine(when: "12 min. ago", text: "check_suite failed on p3-2-retries: woke its session"),
                      IntegrationLogLine(when: "3 hr. ago", text: "Issue #418 labelled herder: started a session in api"),
                      IntegrationLogLine(when: "3 hr. ago", text: "Webhook delivery retried", failed: true)])),
        MockIntegration(
            id: "io.sentry.mcp", name: "Sentry", publisher: "Sentry", trust: .verified, version: "2.1.0", category: .monitoring,
            summary: "Agents look up issues, stack traces and releases while they fix a bug.",
            provides: [.tools], place: .sessions,
            scopes: ["Issues and events: read", "Releases: read"],
            secrets: ["SENTRY_TOKEN"],
            settings: [("Only in sessions of the picked projects", "", true)],
            symbol: "waveform.path.ecg", tint: Theme.avatars[2],
            installed: IntegrationInstall(
                health: .failing, machines: ["studio", "devbox"], projects: ["api"],
                lastEvent: "Refused: the token has expired", problem: ("The token has expired", "Replace Token"),
                log: [IntegrationLogLine(when: "20 min. ago", text: "401 from sentry.io: token expired", failed: true),
                      IntegrationLogLine(when: "yesterday", text: "search_issues called 14 times")])),
        MockIntegration(
            id: "app.linear", name: "Linear", publisher: "Linear", trust: .verified, version: "1.2.0", category: .issues,
            summary: "Assign an issue to herder and a session picks it up; agents update the issue as they go.",
            provides: [.triggers, .tools], place: .hub,
            scopes: ["Issues: read and write", "Webhooks to the hub", "Only the teams you pick"],
            secrets: ["LINEAR_API_KEY"], settings: [], symbol: "line.3.diagonal", tint: Theme.avatars[0]),
        MockIntegration(
            id: "com.atlassian.jira", name: "Jira", publisher: "Atlassian", trust: .verified, version: "0.4.1", category: .issues,
            summary: "Start sessions from Jira issues and keep their status in step.",
            provides: [.triggers, .tools], place: .hub,
            scopes: ["Issues: read and write", "Webhooks to the hub"],
            secrets: ["JIRA_TOKEN"], settings: [], symbol: "square.stack.3d.up", tint: Theme.avatars[3]),
        MockIntegration(
            id: "sh.herder.discord", name: "Discord", publisher: "herder", trust: .builtIn, version: "0.1.0", category: .chat,
            summary: "A Discord bot for your sessions, in the servers and channels you pick.",
            provides: [.chat], place: .machine,
            scopes: ["Read and write messages where it is added"],
            secrets: ["DISCORD_BOT_TOKEN"], settings: [], symbol: "gamecontroller", tint: Theme.avatars[5]),
        MockIntegration(
            id: "com.microsoft.playwright", name: "Playwright", publisher: "Microsoft", trust: .verified, version: "0.0.41",
            category: .tools, summary: "A browser agents drive: open pages, click, take screenshots.",
            provides: [.tools], place: .sessions,
            scopes: ["Runs a browser on the machine", "Reaches any site the machine can"],
            secrets: [], settings: [], symbol: "safari", tint: Theme.avatars[1]),
        MockIntegration(
            id: "com.figma.mcp", name: "Figma", publisher: "Figma", trust: .verified, version: "1.0.3", category: .tools,
            summary: "Agents read frames and design tokens to build what was drawn.",
            provides: [.tools, .skills], place: .sessions,
            scopes: ["Files you share with it: read"],
            secrets: ["FIGMA_TOKEN"], settings: [], symbol: "paintpalette", tint: Theme.avatars[4]),
        MockIntegration(
            id: "dev.community.postgres", name: "Postgres", publisher: "pg-tools", trust: .community, version: "0.3.0",
            category: .tools, summary: "Read-only SQL against a database you name, for agents that debug data.",
            provides: [.tools], place: .sessions,
            scopes: ["Connects to the database in its secret", "Read-only queries"],
            secrets: ["DATABASE_URL"], settings: [], symbol: "cylinder.split.1x2", tint: Theme.avatars[1]),
        MockIntegration(
            id: "com.pagerduty", name: "PagerDuty", publisher: "oncall-kit", trust: .community, version: "0.2.0",
            category: .monitoring, summary: "An incident opens a session with its alerts and runbook.",
            provides: [.triggers], place: .hub,
            scopes: ["Incidents: read", "Webhooks to the hub"],
            secrets: ["PAGERDUTY_TOKEN"], settings: [], symbol: "bell.badge", tint: Theme.avatars[2]),
        MockIntegration(
            id: "local.mcp", name: "Your own MCP server", publisher: "you", trust: .community, version: "—", category: .tools,
            summary: "Any MCP server by command or URL, as Add MCP Server does today.",
            provides: [.tools], place: .sessions,
            scopes: ["Whatever the server does"], secrets: [], settings: [], symbol: "plus.square.dashed",
            tint: Theme.avatars[5]),
    ]
}

/// An integration's tile: its symbol on its tint.
struct IntegrationMark: View {
    let integration: MockIntegration
    var size: CGFloat = 34

    var body: some View {
        Image(systemName: integration.symbol)
            .font(.system(size: size * 0.45, weight: .semibold))
            .foregroundStyle(.white)
            .frame(width: size, height: size)
            .background(integration.tint, in: .rect(cornerRadius: size * 0.26))
            .accessibilityHidden(true)
    }
}

/// Says the screen is a mock, so a reviewer never mistakes it for the feature.
struct MockNotice: View {
    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "flask").foregroundStyle(Theme.secondary)
            Text("Mock · demo data only. Nothing here is installed or reaches a service.")
                .font(Theme.body).foregroundStyle(Theme.secondary)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 10)
        .background(Theme.raised, in: .rect(cornerRadius: Theme.corner))
        .accessibilityIdentifier("integrations-mock")
    }
}

/// The trust line under a name: "✓ Verified · Sentry".
struct TrustLine: View {
    let integration: MockIntegration

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: integration.trust.symbol).imageScale(.small)
                .foregroundStyle(integration.trust == .community ? Theme.tertiary : Theme.action)
            Text("\(integration.trust.label) · \(integration.publisher)")
                .foregroundStyle(Theme.secondary)
                .lineLimit(1)
        }
        .font(Theme.meta)
    }
}

/// The catalog: what is installed, with how each is doing, then what can be, searchable and by
/// category. Inside the demo only (`Fleet.isDemo`).
struct IntegrationsView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    @State private var query = ""
    @State private var category = IntegrationCategory.all
    @Environment(\.pages) private var pages

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                MockNotice()
                filters
                let installed = shown(MockIntegrations.installed)
                if !installed.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        SectionHeading(title: "Installed", count: installed.count)
                        SettingsGroup {
                            ForEach(Array(installed.enumerated()), id: \.element.id) { index, integration in
                                if index > 0 { RowDivider() }
                                InstalledRow(integration: integration) { pages.open(.integration(integration.id)) }
                                    .accessibilityIdentifier("integration-\(integration.id)")
                            }
                        }
                    }
                }
                let available = shown(MockIntegrations.available)
                VStack(alignment: .leading, spacing: 8) {
                    SectionHeading(title: "Available", count: available.count)
                    if available.isEmpty {
                        Text("Nothing in the catalog matches.").font(Theme.body).foregroundStyle(Theme.tertiary)
                    }
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 260), spacing: 12)], spacing: 12) {
                        ForEach(available) { integration in
                            CatalogCard(integration: integration) {
                                pages.open(.integration(integration.id))
                            } install: {
                                sheet = .installIntegration(integration.id)
                            }
                        }
                    }
                }
            }
            .padding(.top, 4)
            .padding(.bottom, 28)
            .pageColumn()
        }
        .background(.ground)
        #if os(iOS)
        .navigationTitle("Integrations")
        #endif
    }

    private var filters: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").foregroundStyle(Theme.tertiary)
                TextField("Search integrations", text: $query)
                    .textFieldStyle(.plain)
                    .foregroundStyle(Theme.text)
            }
            .font(.subheadline)
            .padding(.horizontal, 12)
            .frame(height: 34)
            .background(.card, in: .rect(cornerRadius: Theme.corner))
            ScrollView(.horizontal) {
                HStack(spacing: 6) {
                    ForEach(IntegrationCategory.allCases, id: \.self) { option in
                        let selected = option == category
                        Button { category = option } label: {
                            Text(option.rawValue)
                                .font(Theme.meta.weight(.medium))
                                .foregroundStyle(selected ? Theme.text : Theme.secondary)
                                .padding(.horizontal, 10)
                                .padding(.vertical, 5)
                                .primaryFill(in: .capsule, on: selected)
                                .hitTarget()
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
            .scrollIndicators(.never)
        }
    }

    private func shown(_ integrations: [MockIntegration]) -> [MockIntegration] {
        let needle = query.trimmingCharacters(in: .whitespaces)
        return integrations.filter { integration in
            (category == .all || integration.category == category)
                && (needle.isEmpty || integration.name.localizedCaseInsensitiveContains(needle)
                    || integration.summary.localizedCaseInsensitiveContains(needle)
                    || integration.publisher.localizedCaseInsensitiveContains(needle))
        }
    }
}

/// An installed integration in the catalog: its tile, name, what it last did, and its health.
private struct InstalledRow: View {
    let integration: MockIntegration
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 12) {
                IntegrationMark(integration: integration)
                VStack(alignment: .leading, spacing: 2) {
                    Text(integration.name).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                    Text(detail).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                }
                .layoutPriority(1)
                Spacer(minLength: 8)
                if let health = integration.installed?.health {
                    StatusChip(text: health.chip, urgency: health.urgency)
                }
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
            }
            .padding(.horizontal, Theme.cellPadding)
            .padding(.vertical, 8)
            .frame(minHeight: Theme.tallRowHeight)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }

    /// "Chat bridge · studio · Started a session in #web 4 min. ago".
    private var detail: String {
        guard let installed = integration.installed else { return integration.providesLine }
        return ([integration.providesLine, installed.machines.joined(separator: ", "), installed.lastEvent])
            .joined(separator: " · ")
    }
}

/// An integration the catalog offers: its tile, name and trust, one line of what it does, what
/// it provides, and Install.
private struct CatalogCard: View {
    let integration: MockIntegration
    let open: () -> Void
    let install: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Button(action: open) {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(spacing: 10) {
                        IntegrationMark(integration: integration)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(integration.name).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                                .lineLimit(1)
                            TrustLine(integration: integration)
                        }
                        Spacer(minLength: 0)
                    }
                    Text(integration.summary).font(.caption).foregroundStyle(Theme.secondary)
                        .lineLimit(2, reservesSpace: true)
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            HStack(spacing: 6) {
                ForEach(integration.provides, id: \.self) { capability in
                    Chip(symbol: capability.symbol, text: capability.title)
                }
                Spacer(minLength: 4)
                PillButton(title: "Install", prominent: true, action: install)
                    .accessibilityIdentifier("install-\(integration.id)")
            }
        }
        .padding(Theme.cellPadding)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.card, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
    }
}

/// One integration's page: how it is doing and its fix, what it provides and may touch, its
/// settings, where it runs, what it did lately, then its identity and Remove. An integration not
/// installed shows what it would ask for, with Install.
struct IntegrationPage: View {
    let fleet: Fleet
    let integrationId: String
    @Binding var sheet: AppSheet?
    @State private var switches: [String: Bool] = [:]

    var body: some View {
        if let integration = MockIntegrations.find(integrationId) {
            page(integration)
        } else {
            ContentUnavailableView("No such integration", systemImage: "puzzlepiece.extension")
        }
    }

    private func page(_ integration: MockIntegration) -> some View {
        let installed = integration.installed
        return DetailPage(
            title: integration.name,
            chip: installed.map { StatusChip(text: $0.health.chip, urgency: $0.health.urgency) },
            status: "\(integration.trust.label) · \(integration.publisher) · v\(integration.version) · \(integration.place.label)"
        ) {
            IntegrationMark(integration: integration, size: 40)
        } actions: {
            if installed == nil {
                PaneButton(title: "Install", symbol: "arrow.down.circle", prominent: true) {
                    sheet = .installIntegration(integration.id)
                }
            } else {
                PaneButton(title: "Configure", symbol: "slider.horizontal.3") {
                    sheet = .installIntegration(integration.id)
                }
            }
        } content: {
            MockNotice()
            if let installed, let problem = installed.problem {
                NeedsAttention {
                    AttentionRow(title: problem.title, detail: installed.lastEvent,
                                 urgency: installed.health == .failing ? .failing : .needsYou) {
                        PillButton(title: problem.fix, prominent: true) { mockOnly() }
                    }
                }
            }
            SettingsList(title: "What it provides") {
                ForEach(Array(integration.provides.enumerated()), id: \.element) { index, capability in
                    if index > 0 { RowDivider() }
                    capabilityRow(capability, of: integration)
                }
            }
            .detailSection("What it provides")
            SettingsList(title: "What it can access",
                         caption: "Asked for when it is installed; anything more asks again.") {
                ForEach(Array(integration.scopes.enumerated()), id: \.offset) { index, scope in
                    if index > 0 { RowDivider() }
                    Label(scope, systemImage: "checkmark")
                        .font(.subheadline).foregroundStyle(Theme.text)
                        .padding(.horizontal, Theme.cellPadding)
                        .frame(maxWidth: .infinity, minHeight: Theme.rowHeight, alignment: .leading)
                }
                if !integration.secrets.isEmpty {
                    RowDivider()
                    SettingRow(label: "Secrets", hint: "Kept on the hub; the integration gets them at run time, never the app",
                               help: "Each is a hub secret, as MCP servers' are today. herder never sees provider logins.") {
                        Text(integration.secrets.joined(separator: ", ")).font(Theme.monoSmall)
                            .foregroundStyle(Theme.secondary).lineLimit(1)
                    }
                }
                if let installed {
                    RowDivider()
                    SettingRow(label: "Projects", hint: "The only ones it may start sessions in or read") {
                        Text(installed.projects.isEmpty ? "Chats only" : installed.projects.joined(separator: ", "))
                            .font(.subheadline).foregroundStyle(Theme.secondary).lineLimit(1)
                    }
                }
            }
            .detailSection("What it can access")
            if installed != nil, !integration.settings.isEmpty {
                SettingsList(title: "Settings") {
                    ForEach(Array(integration.settings.enumerated()), id: \.offset) { index, setting in
                        if index > 0 { RowDivider() }
                        SwitchRow(title: setting.title, detail: setting.detail, isOn: Binding(
                            get: { switches[setting.title] ?? setting.on },
                            set: { switches[setting.title] = $0 }))
                    }
                }
                .detailSection("Settings")
            }
            if let installed {
                SettingsList(title: integration.place == .hub ? "Runs on" : "Machines") {
                    ForEach(Array(installed.machines.enumerated()), id: \.offset) { index, machine in
                        if index > 0 { RowDivider() }
                        SettingRow(label: machine, hint: machineHint(integration)) {
                            StatusChip(text: installed.health == .failing ? "Failing" : "Running",
                                       urgency: installed.health == .failing ? .failing : .done)
                        }
                    }
                    if integration.place != .hub {
                        RowDivider()
                        AddRow(title: "Add Machine…") { mockOnly() }
                    }
                }
                .detailSection("Machines")
                VStack(alignment: .leading, spacing: 8) {
                    SectionHeading(title: "Activity")
                    SettingsGroup {
                        ForEach(Array(installed.log.enumerated()), id: \.offset) { index, line in
                            if index > 0 { RowDivider() }
                            HStack(alignment: .firstTextBaseline, spacing: 10) {
                                Circle().fill(line.failed ? Theme.failure : Theme.tertiary).frame(width: 6, height: 6)
                                Text(line.text).font(.subheadline).foregroundStyle(Theme.text).lineLimit(2)
                                Spacer(minLength: 8)
                                Text(line.when).font(Theme.meta).foregroundStyle(Theme.tertiary)
                            }
                            .padding(.horizontal, Theme.cellPadding)
                            .padding(.vertical, 10)
                        }
                    }
                }
                .detailSection("Activity")
            }
            AboutSection {
                DetailRow(label: "Id", value: integration.id, mono: true)
                DetailRow(label: "Version", value: integration.version, mono: true)
                DetailRow(label: "Publisher", value: "\(integration.publisher) (\(integration.trust.label.lowercased()))")
                DetailRow(label: "Runs", value: integration.place.label)
            }
            if installed != nil {
                DangerZone {
                    SettingRow(label: "Remove \(integration.name)",
                               hint: "Stops it everywhere; its secrets stay on the hub until you delete them") {
                        DangerButton(title: "Remove…") { mockOnly() }
                    }
                }
            }
        }
    }

    private func capabilityRow(_ capability: IntegrationCapability, of integration: MockIntegration) -> some View {
        HStack(spacing: 12) {
            Image(systemName: capability.symbol).foregroundStyle(Theme.secondary).frame(width: 22)
            VStack(alignment: .leading, spacing: 2) {
                Text(capability.title).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                Text(Self.explain(capability, integration)).font(.caption).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, Theme.cellPadding)
        .padding(.vertical, 8)
        .frame(minHeight: Theme.tallRowHeight)
    }

    private static func explain(_ capability: IntegrationCapability, _ integration: MockIntegration) -> String {
        switch capability {
        case .chat: "People you allow talk to sessions from \(integration.name); their conversations show under Bots."
        case .tools: "An MCP server sessions get, in the projects you pick."
        case .triggers: "\(integration.name)'s events reach the hub and start or wake sessions."
        case .skills: "Adds its skills to the library."
        }
    }

    private func machineHint(_ integration: MockIntegration) -> String {
        switch integration.place {
        case .machine: "The bot connects from here"
        case .hub: "Receives the webhooks"
        case .sessions: "Its sessions get the tools"
        }
    }

    private func mockOnly() {
        fleet.toast = Toast(text: "This is a mock: nothing changed")
    }
}

/// Install, or configure, an integration: what it may do, where it runs, its secrets and the
/// projects it may touch. A mock: Install changes nothing.
struct InstallIntegrationSheet: View {
    let fleet: Fleet
    let integrationId: String
    @State private var machine = "studio"
    @State private var secrets: [String: String] = [:]
    @State private var projects: Set<String> = ["api"]
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let integration = MockIntegrations.find(integrationId)
        let name = integration?.name ?? "Integration"
        let installed = integration?.installed != nil
        SheetScaffold(title: installed ? "Configure \(name)" : "Install \(name)",
                      subtitle: integration.map { "\($0.trust.label) · \($0.publisher) · v\($0.version)" } ?? "",
                      hint: "Mock: nothing is installed. Secrets go to the hub, never to this device.") {
            MockNotice()
            if let integration {
                NumberedStep(number: 1, title: "It will be able to") {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(integration.scopes, id: \.self) { scope in
                            Label(scope, systemImage: "checkmark").font(.subheadline).foregroundStyle(Theme.text)
                        }
                    }
                }
                NumberedStep(number: 2, title: "Where it runs") {
                    if integration.place == .machine {
                        ChoiceChips(options: [("studio", "studio", "macOS · owner"), ("laptop", "laptop", "macOS · owner"),
                                              ("devbox", "devbox", "Linux · owner")], selection: $machine)
                    } else {
                        Text(integration.place == .hub
                             ? "On the hub: it is always on, so webhooks reach it."
                             : "With each session that uses it, on the machine the session runs on.")
                            .font(.subheadline).foregroundStyle(Theme.secondary)
                    }
                }
                if !integration.secrets.isEmpty {
                    NumberedStep(number: 3, title: "Secrets") {
                        ForEach(integration.secrets, id: \.self) { secret in
                            Field(label: secret) {
                                InputBox(placeholder: "Paste the value", text: Binding(
                                    get: { secrets[secret] ?? "" }, set: { secrets[secret] = $0 }), mono: true,
                                         secure: true)
                            }
                        }
                    }
                }
                NumberedStep(number: integration.secrets.isEmpty ? 3 : 4, title: "Projects it may touch") {
                    SettingsGroup {
                        ForEach(Array(["api", "web", "site"].enumerated()), id: \.offset) { index, project in
                            if index > 0 { RowDivider() }
                            SwitchRow(title: project, isOn: Binding(
                                get: { projects.contains(project) },
                                set: { if $0 { projects.insert(project) } else { projects.remove(project) } }))
                        }
                    }
                }
            }
        } footer: {
            Spacer()
            Button(installed ? "Save" : "Install") {
                fleet.toast = Toast(text: "This is a mock: nothing was installed")
                dismiss()
            }
            .buttonStyle(.plain)
            .font(.body.weight(.semibold))
            .foregroundStyle(Theme.text)
            .padding(.horizontal, 16)
            .frame(height: 36)
            .primaryFill(in: .rect(cornerRadius: 8))
        }
    }
}
