import Herder
import SwiftUI

/// A session: its transcript live, what it asks pinned above the composer, and the controls
/// to prompt, interrupt, answer and switch.
struct SessionView: View {
    let fleet: Fleet
    let key: SessionKey
    /// Opens another session (a child); `nil` pushes it.
    var open: ((SessionKey) -> Void)?
    @State private var forking = false
    /// The machine the fork sheet opens on, when picked from a menu.
    @State private var forkTarget: HostId?
    @State private var completedFork: SessionKey?
    @State private var pushedFork: SessionKey?
    @State private var switching = false
    @State private var showsTerminal = false
    @State private var showsPRs = false
    @AppStorage("listHidden") private var listHidden = false
    @AppStorage("inspectorShown") private var inspectorShown = false
    /// The inspector as a sheet, on compact width; not kept, unlike the pane.
    @State private var inspectorSheet = false
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    private var compact: Bool { sizeClass == .compact }
    #else
    private let compact = false
    #endif
    private var inspector: InspectorPresentation { InspectorPresentation(compact: compact) }
    @State private var linking = false
    @State private var typedPR = ""
    /// Where the transcript is scrolled, and where each session shown here was left.
    @State private var scroll = TranscriptScroll()
    /// Bumped on every send, so the transcript jumps to its end.
    @State private var sent = 0

    var body: some View {
        let model = fleet.sessions[key]
        let summary = fleet.lists.projects.lazy.flatMap(\.sessions).first { $0.key == key }
        let blocks = model.map(Transcript.blocks) ?? []
        VStack(spacing: 0) {
            if let parent = model?.parent {
                ChildBanner(fleet: fleet, parent: SessionKey(hostId: key.hostId, sessionId: parent), open: open)
            }
            header(model, summary)
                .overlay(alignment: .leading) {
                    if model?.parent != nil { Rectangle().fill(Theme.child).frame(width: 3) }
                }
            Rectangle().fill(Theme.stroke).frame(height: 1)
            if showsTerminal {
                TerminalPane(fleet: fleet, key: key)
            } else {
            ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 16) {
                    if model?.loaded != true {
                        ProgressView().tint(Theme.secondary).frame(maxWidth: .infinity).padding(40)
                    } else if blocks.isEmpty {
                        Text("No turns yet. Send a prompt to start.").foregroundStyle(Theme.tertiary)
                            .frame(maxWidth: .infinity).padding(40)
                    }
                    ForEach(blocks) { block in
                        TranscriptBlockView(block: block, fleet: fleet, key: key, open: open)
                    }
                }
                .scrollTargetLayout()
                .frame(maxWidth: 760)
                .frame(maxWidth: .infinity)
                .padding(16)
                .overlay(alignment: .bottom) { Color.clear.frame(height: 1).id(TranscriptScroll.end) }
                .onAppear { if let top = scroll.land() { proxy.scrollTo(top, anchor: .top) } }
            }
            .defaultScrollAnchor(.bottom)
            .scrollDismissesKeyboard(.interactively)
            .accessibilityIdentifier("transcript")
            .modifier(FollowsGrowth(key: key, scroll: $scroll))
            .id(key)
            .onChange(of: sent) { withAnimation { proxy.scrollTo(TranscriptScroll.end, anchor: .bottom) } }
            }
            if let model {
                controls(model, summary)
            }
            }
        }
        .background(Theme.background)
        .overlay(alignment: .trailing) { EmptyView() }
        .safeAreaInset(edge: .trailing, spacing: 0) {
            if inspector == .pane && inspectorShown {
                HStack(spacing: 0) {
                    Rectangle().fill(Theme.stroke).frame(width: 1)
                    SessionInspector(fleet: fleet, key: key).frame(width: 300)
                }
            }
        }
        .onChange(of: key) { old, _ in
            showsTerminal = false
            scroll.show(key)
            fleet.unwatch(old)
            fleet.watch(key)
        }
        .onAppear { fleet.watch(key) }
        .onDisappear { fleet.unwatch(key) }
        .sheet(isPresented: $forking, onDismiss: {
            guard let key = completedFork else { return }
            completedFork = nil
            if let open { open(key) } else { pushedFork = key }
        }) {
            ForkSessionSheet(fleet: fleet, key: key, preselect: forkTarget) { completedFork = $0 }
        }
        .navigationDestination(isPresented: Binding(
            get: { pushedFork != nil }, set: { if !$0 { pushedFork = nil } }
        )) {
            if let key = pushedFork { SessionView(fleet: fleet, key: key) }
        }
        .sheet(isPresented: $switching) { SwitchSheet(fleet: fleet, key: key) }
        .sheet(isPresented: $inspectorSheet) {
            SessionInspector(fleet: fleet, key: key)
                .presentationDetents([.medium, .large])
                .presentationBackground(Theme.surface)
        }
        .alert("Link a pull request", isPresented: $linking) {
            TextField("123, #123 or a link", text: $typedPR)
            Button("Link") {
                if let number = prNumber(in: typedPR) { Task { await fleet.linkPR(number, to: key) } }
                typedPR = ""
            }
            Button("Cancel", role: .cancel) { typedPR = "" }
        }
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        #endif
    }

    // MARK: Header

    /// The header, with the buttons' labels where they fit beside 160 points of title, else
    /// the buttons as icons, so a label never truncates.
    private func header(_ model: SessionModel?, _ summary: SessionSummary?) -> some View {
        ViewThatFits(in: .horizontal) {
            headerRow(model, summary, labels: true)
            headerRow(model, summary, labels: false)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }

    private func headerRow(_ model: SessionModel?, _ summary: SessionSummary?, labels: Bool) -> some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 10) {
                    if model?.parent != nil { ChildAvatar(session: model, size: 26, showsState: false) }
                    Text(summary?.title ?? model?.title ?? "Session")
                        .font(.title3.weight(.bold)).foregroundStyle(Theme.text).lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
                // One line, so a narrow header truncates where it runs on rather than wrapping
                // its parts beside each other.
                HStack(spacing: 6) {
                    StatusGlyph(state: model?.state ?? .idle, size: 7)
                    (Text(model?.state.label ?? "")
                        .foregroundStyle(model?.state == .needsYou ? Theme.accent : Theme.secondary)
                     + Text(summary.map { " · \($0.project.isEmpty ? "" : $0.project + " · ")\($0.machine)" } ?? "")
                        .foregroundStyle(Theme.tertiary))
                        .lineLimit(1)
                }
                .font(.footnote.weight(.medium))
                if let origin = fleet.forkOrigins[key] {
                    Text("Forked from \(origin.sessionId) on \(origin.hostId)")
                        .font(.caption).foregroundStyle(Theme.secondary).textSelection(.enabled)
                }
                if let branch = model?.branch {
                    Text(branch).font(Theme.monoSmall).foregroundStyle(Theme.tertiary).textSelection(.enabled)
                        .lineLimit(1).truncationMode(.middle)
                }
            }
            .frame(idealWidth: 160, maxWidth: .infinity, alignment: .leading)
            HStack(spacing: 6) {
            if let model, !model.prs.isEmpty {
                HeaderButton(symbol: "arrow.triangle.pull", title: model.prs.count == 1 ? "#\(model.prs[0].number)" : "\(model.prs.count) PRs",
                             tint: model.prs.sorted { $0.state.rank < $1.state.rank }.first?.state.color ?? Theme.secondary) {
                    showsPRs.toggle()
                }
                .popover(isPresented: $showsPRs, arrowEdge: .bottom) {
                    PRStrip(fleet: fleet, key: key, prs: model.prs)
                        .frame(width: 520)
                        .background(Theme.surface)
                        .preferredColorScheme(.dark)
                }
            }
            if fleet.machines.first(where: { $0.hostId == key.hostId })?.role == .owner, model?.state != .archived {
                HeaderButton(symbol: showsTerminal ? "text.bubble" : "terminal",
                             title: labels ? showsTerminal ? "Chat" : "Terminal" : nil,
                             selected: showsTerminal) { showsTerminal.toggle() }
                    .accessibilityLabel(showsTerminal ? "Chat" : "Terminal")
                    .keyboardShortcut("`", modifiers: .command)
                    .help(showsTerminal ? "Back to the chat (⌘`)" : "A shell in this session's worktree (⌘`)")
            }
            HeaderButton(symbol: "info.circle", title: nil, selected: inspector == .pane && inspectorShown) {
                switch inspector {
                case .pane: inspectorShown.toggle()
                case .sheet: inspectorSheet = true
                }
            }
                .keyboardShortcut("i", modifiers: [.command, .option])
                .help("Events and statistics (⌥⌘I)")
            #if os(macOS)
            HeaderButton(symbol: listHidden ? "sidebar.squares.left" : "rectangle.expand.vertical",
                         title: nil) { listHidden.toggle() }
                .keyboardShortcut("\\", modifiers: .command)
                .help(listHidden ? "Show the session list (⌘\\)" : "Give the session the whole width (⌘\\)")
            #endif
            if fleet.archiving.contains(key) {
                ArchivingLabel()
            }
            if let model {
                Menu {
                    if model.state != .archived {
                        if model.turn != nil {
                            Button("Interrupt", systemImage: "stop.circle") { Task { await fleet.interrupt(key) } }
                        }
                        Button("Switch Account or Model…", systemImage: "arrow.left.arrow.right") { switching = true }
                        Button("Link Pull Request…", systemImage: "link") { linking = true }
                        Divider()
                        Button("Archive", systemImage: "archivebox") { Task { await fleet.archive(key) } }
                            .disabled(fleet.archiving.contains(key))
                    }
                } label: {
                    HeaderLabel(symbol: "ellipsis", title: nil)
                }
                .menuStyle(.button)
                .buttonStyle(.plain)
                .menuIndicator(.hidden)
                .fixedSize()
            }
            }
            // The buttons keep their size; the title, status and branch take what is left.
            .fixedSize()
        }
    }

    // MARK: Controls

    @ViewBuilder
    private func controls(_ model: SessionModel, _ summary: SessionSummary?) -> some View {
        VStack(spacing: 8) {
            if let request = pinned(model, summary) {
                RequestCard(request: request.request, fleet: fleet, showsSession: false, more: request.more)
            }
            if let readOnly = readOnly(model, summary) {
                Label(readOnly, systemImage: "lock")
                    .font(.footnote.weight(.medium))
                    .foregroundStyle(summary?.machineOffline == true ? Theme.failure : Theme.secondary)
                    .frame(maxWidth: .infinity, minHeight: 44)
                    .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
            } else {
                Composer(fleet: fleet, key: key, model: model, sent: { sent += 1 }) { host in
                    forkTarget = host
                    forking = true
                }
            }
        }
        .frame(maxWidth: 784)
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 12)
        .padding(.top, 8)
        .padding(.bottom, 10)
        .background(Theme.background)
    }

    /// The oldest approval, else the oldest question, on any route, as the TUI pins them.
    private func pinned(_ model: SessionModel, _ summary: SessionSummary?) -> (request: PendingRequest, more: Int)? {
        guard let summary, let pending = model.approvals.first ?? model.questions.first else { return nil }
        let kind: PendingRequest.Kind = switch pending.kind {
        case .approval(let summary): .approval(summary: summary)
        case .question(let text, let choices): .question(text: text, choices: choices)
        }
        let reason = pending.routedTo == .primary
            ? "Asked the primary session first; you can still answer"
            : pending.reason?.text
        return (PendingRequest(requestId: pending.id, session: summary, kind: kind, since: pending.since,
                               age: Timestamp.age(pending.since, now: .now), reason: reason, note: pending.note),
                model.approvals.count + model.questions.count - 1)
    }

    /// Why the session cannot be driven from here, if it cannot.
    private func readOnly(_ model: SessionModel, _ summary: SessionSummary?) -> String? {
        switch model.state {
        case .moved: return "Moved to another host · read-only here"
        default: return summary?.machineOffline == true ? "\(summary?.machine ?? "The host") is offline · read-only" : nil
        }
    }
}

/// Keeps the transcript at its end as it grows, and tracks where it is scrolled, where the OS
/// supports it.
private struct FollowsGrowth: ViewModifier {
    let key: SessionKey
    @Binding var scroll: TranscriptScroll

    func body(content: Content) -> some View {
        if #available(iOS 18, macOS 15, *) {
            content
                .defaultScrollAnchor(.bottom, for: .sizeChanges)
                .onScrollTargetVisibilityChange(idType: String.self, threshold: 0.01) { scroll.saw($0.first, in: key) }
                .onScrollGeometryChange(for: Bool.self) { geometry in
                    geometry.visibleRect.maxY >= geometry.contentSize.height - 40
                } action: { _, atEnd in
                    scroll.scrolled(key, atEnd: atEnd)
                }
        } else {
            content
        }
    }
}

/// Where each session's transcript was scrolled, so coming back to one lands where it was left.
/// It is scrolled there once, not bound as the scroll position, so the view still follows
/// new output and sends: a bound position pins its block in place as the transcript grows.
struct TranscriptScroll {
    /// The id of the transcript's end, where a send scrolls.
    static let end = "transcript-end"

    /// The block at the top of each session's view, and the sessions scrolled up from the end.
    private var tops: [SessionKey: String] = [:]
    private var scrolledUp: Set<SessionKey> = []
    /// Where the session being shown again lands, until its transcript appears.
    private var landing: String?

    mutating func saw(_ top: String?, in key: SessionKey) { tops[key] = top }

    mutating func scrolled(_ key: SessionKey, atEnd: Bool) {
        if atEnd { scrolledUp.remove(key) } else { scrolledUp.insert(key) }
    }

    /// Shows a session again: back where it was left scrolled up, else at its end, following
    /// what comes. Decided now, as its new transcript reports itself at the end first.
    mutating func show(_ key: SessionKey) {
        landing = scrolledUp.contains(key) ? tops[key] : nil
    }

    /// The block to scroll to the top as the transcript appears, once.
    mutating func land() -> String? {
        defer { landing = nil }
        return landing
    }
}

/// The prompt box of a session: model and permissions in the box, where it runs and on which
/// account under it; with a question pending it answers it.
private struct Composer: View {
    let fleet: Fleet
    let key: SessionKey
    let model: SessionModel
    /// Called on every send.
    let sent: () -> Void
    /// Opens the fork sheet, on a machine when one was picked.
    let fork: (HostId?) -> Void
    @State private var text = ""
    @State private var images: [Herder.Image] = []

    var body: some View {
        let machine = fleet.machines.first { $0.hostId == key.hostId }
        let accounts = machine?.accounts ?? []
        let account = accounts.first { $0.accountId == model.accountId }
        // The provider is the session's; the Switch sheet moves it to another.
        let current = ModelCatalog.Choice(provider: model.provider ?? "", model: model.model ?? "")
        VStack(alignment: .leading, spacing: 6) {
            if model.state == .archived {
                Label("Archived · sending a message brings it back", systemImage: "archivebox")
                    .font(.caption).foregroundStyle(Theme.tertiary).padding(.horizontal, 18)
            }
            let queue = fleet.queue(of: key)
            if !queue.isEmpty {
                QueueTray(fleet: fleet, key: key, queue: queue, running: model.turn != nil)
            }
            ComposerBox(
                text: $text,
                images: $images,
                placeholder: placeholder,
                tint: model.parent != nil ? Theme.child : nil,
                models: fleet.modelGroups(on: key.hostId, providers: model.provider.map { [$0] } ?? [],
                                          current: current, offersDefault: false),
                current: current,
                mode: model.mode,
                running: model.turn != nil,
                choose: { choice in
                    guard let account else { return }
                    Task { await fleet.switchSession(key, to: account, model: choice.model) }
                },
                settings: [accountSection(accounts), machineSection],
                setMode: { mode in Task { await fleet.setMode(mode, of: key) } },
                send: send,
                stop: { Task { await fleet.interrupt(key) } }
            ) {
                FooterMenu(section: machineSection, text: machine?.name ?? "Machine",
                           help: "Where it runs; pick another machine to fork onto it")
                FooterMenu(section: accountSection(accounts), text: account?.label ?? model.accountId ?? "")
                Spacer()
                if let branch = model.branch {
                    Label(branch, systemImage: "arrow.triangle.branch").lineLimit(1)
                }
            }
            if let refusal = fleet.refusals[key] {
                Text(refusal).font(.footnote).foregroundStyle(Theme.failure).padding(.horizontal, 18)
            }
        }
    }

    /// The provider's accounts on the machine; picking one moves the session to it.
    private func accountSection(_ accounts: [Account]) -> SettingsSection {
        SettingsSection(
            kind: .account,
            options: SettingsOption.accounts(accounts, provider: model.provider, current: model.accountId)
        ) { id in
            guard let other = accounts.first(where: { $0.accountId == id }) else { return }
            Task { await fleet.switchSession(key, to: other, model: "") }
        }
    }

    /// The machines; picking another opens the fork sheet on it.
    private var machineSection: SettingsSection {
        SettingsSection(
            kind: .machine,
            options: SettingsOption.machines(fleet.machines, current: key.hostId) { machine in
                forkable ? ForkSessionModel.ineligible(machine, source: key, provider: model.provider) : "Cannot fork"
            },
            hint: "Another forks onto it",
            action: .init(title: "Fork Session…", symbol: "arrow.triangle.branch", enabled: forkable) { fork(nil) }
        ) { fork($0) }
    }

    /// A child session stays with its parent; a session forks once it has loaded.
    private var forkable: Bool { model.loaded && model.parent == nil }

    private var placeholder: String {
        if !model.questions.isEmpty { return "Type an answer…" }
        if model.parent != nil {
            return model.turn != nil ? "Queue a message for this child session…" : "Message this child session…"
        }
        return model.turn != nil ? "Queue a follow-up…" : "Ask for changes or send a follow-up"
    }

    private func send() {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let images = images
        guard !text.isEmpty || !images.isEmpty else { return }
        self.text = ""
        self.images = []
        sent()
        if model.state == .archived {
            Task { await fleet.unarchiveAndSubmit(text, images: images, to: key) }
        } else {
            Task { await fleet.submit(text, images: images, to: key) }
        }
    }
}

/// Moves a session to another account or model: the machine's accounts with their busiest
/// usage window, and a model.
struct SwitchSheet: View {
    let fleet: Fleet
    let key: SessionKey
    @Environment(\.dismiss) private var dismiss
    @State private var accountId: AccountId = ""
    @State private var model = ""

    var body: some View {
        let session = fleet.sessions[key]
        let accounts = fleet.machines.first { $0.hostId == key.hostId }?.accounts ?? []
        let chosen = accounts.first { $0.accountId == accountId }
        SheetScaffold(title: "Switch", subtitle: "Move the session to another account, model or provider.", height: 520) {
            Field(label: "Account") {
                ChoiceChips(options: accounts.map { account in
                    let busiest = account.usage.max { $0.usedPercent < $1.usedPercent }
                    let usage = busiest.map { "\(Lists.usageLabel($0.window)) \(Int($0.usedPercent.rounded()))%" } ?? ""
                    let current = account.accountId == session?.accountId ? "current · " : ""
                    return (account.accountId, account.label, "\(current)\(account.provider)\(usage.isEmpty ? "" : " · " + usage)")
                }, selection: $accountId)
            }
            Field(label: "Model", hint: chosen?.provider == session?.provider
                  ? "Blank keeps \(session?.model ?? "the model")."
                  : "Blank uses the provider's default.") {
                InputBox(placeholder: chosen?.provider == session?.provider ? "keep \(session?.model ?? "")" : "Provider's default",
                         text: $model, mono: true)
            }
            if let refusal = fleet.refusals[key] {
                Text(refusal).font(.footnote).foregroundStyle(Theme.failure)
            }
        } footer: {
            Spacer()
            ActionButton(title: "Switch", style: .primary) {
                guard let chosen else { return }
                await fleet.switchSession(key, to: chosen, model: model)
                if fleet.refusals[key] == nil { dismiss() }
            }
            .frame(maxWidth: 200)
            .keyboardShortcut(.defaultAction)
        }
        .onAppear { accountId = session?.accountId ?? accounts.first?.accountId ?? "" }
    }
}

/// A new session's empty chat: what it asks, the prompt box with the model and permissions
/// preselected, and where it runs under it; the first message creates the session.
struct DraftSessionView: View {
    let fleet: Fleet
    let draft: Draft
    let created: (SessionKey) -> Void
    @State private var hostId: HostId = ""
    /// The provider and model it starts on; picking another provider's model switches to it.
    @State private var choice = ModelCatalog.Choice(provider: "", model: "")
    @State private var mode: PermissionMode = .fullAccess
    @State private var text = ""
    @State private var images: [Herder.Image] = []
    @State private var error: String?
    /// The first message while the session is being created.
    @State private var starting: String?

    private var machine: Machine? { fleet.machines.first { $0.hostId == hostId } }
    /// Machines the draft can run on: the connected ones with the project, or any for a path.
    private var machines: [Machine] {
        fleet.machines.filter { machine in
            machine.connection == .connected && machine.hosts.isEmpty
                && (draft.projectId == nil || machine.projects.contains { $0.projectId == draft.projectId })
        }
    }
    private var place: String {
        machine?.projects.first { $0.projectId == draft.projectId }?.name
            ?? draft.repo.map { URL(fileURLWithPath: $0).lastPathComponent } ?? ""
    }

    var body: some View {
        VStack(spacing: 28) {
            Spacer()
            if let starting {
                VStack(alignment: .trailing, spacing: 10) {
                    Text(starting)
                        .foregroundStyle(Theme.onBubble)
                        .padding(.horizontal, 14).padding(.vertical, 10)
                        .background(Theme.bubble, in: .rect(cornerRadius: 18))
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small).tint(Theme.secondary)
                        Text("Starting a session on \(machine?.name ?? "the machine")…").foregroundStyle(Theme.secondary)
                    }
                    .font(.footnote)
                }
                .frame(maxWidth: 760, alignment: .trailing)
            } else {
            Text("What should we build in \(machine?.name ?? "")/\(place)?")
                .font(.system(size: 30, weight: .medium))
                .foregroundStyle(Theme.text)
                .multilineTextAlignment(.center)
            ComposerBox(
                text: $text,
                images: $images,
                placeholder: "Ask for changes, or describe what to build",
                models: fleet.modelGroups(on: hostId, providers: fleet.providers(on: hostId), current: choice,
                                          offersDefault: true),
                current: choice,
                mode: mode,
                running: false,
                choose: { choice = $0 },
                settings: [machineSection],
                setMode: { mode = $0 },
                send: { Task { await start() } },
                stop: {}
            ) {
                FooterMenu(section: machineSection, text: machine?.name ?? "", help: "Where it runs")
                Label("New worktree", systemImage: "folder.badge.plus")
                Spacer()
                Label("From the default branch", systemImage: "arrow.triangle.branch")
            }
            .frame(maxWidth: 760)
            }
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
            }
            Spacer()
        }
        .padding(.horizontal, 24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
        .onAppear {
            hostId = draft.hostId
            choice = fleet.draftChoice(on: hostId, projectId: draft.projectId)
            mode = fleet.machines.first { $0.hostId == draft.hostId }?.projects
                .first { $0.projectId == draft.projectId }?.defaultPermissionMode ?? ModePreference.mode(for: draft)
        }
    }

    /// The machines it can start on; picking one moves the draft there.
    private var machineSection: SettingsSection {
        SettingsSection(kind: .machine, options: SettingsOption.machines(machines, current: hostId) { _ in nil }) { id in
            hostId = id
            choice = fleet.draftChoice(choice, movedTo: id, projectId: draft.projectId)
        }
    }

    private func start() async {
        let prompt = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !prompt.isEmpty || !images.isEmpty else { return }
        guard let account = fleet.defaultAccount(on: hostId, projectId: draft.projectId, provider: choice.provider) else {
            error = "\(machine?.name ?? "This machine") has no \(ModelCatalog.providerName(choice.provider)) account."
            return
        }
        ModePreference.remember(mode, for: draft)
        starting = prompt
        defer { starting = nil }
        do {
            created(try await fleet.createSession(
                on: hostId, repo: draft.createArguments.repo, projectId: draft.createArguments.projectId, accountId: account.accountId,
                model: choice.model, mode: mode, prompt: prompt, images: images))
        } catch {
            self.error = describe(error)
        }
    }
}

/// The permission mode new sessions of a project start in, remembered on this device; full
/// access until changed.
enum ModePreference {
    private static func key(_ draft: Draft) -> String { "mode." + (draft.projectId ?? draft.repo ?? "") }

    static func mode(for draft: Draft) -> PermissionMode {
        switch UserDefaults.standard.string(forKey: key(draft)) {
        case "read_only": .readOnly
        case "ask": .ask
        case "auto_edit": .autoEdit
        default: .fullAccess
        }
    }

    static func remember(_ mode: PermissionMode, for draft: Draft) {
        let value = switch mode {
        case .readOnly: "read_only"
        case .ask: "ask"
        case .autoEdit: "auto_edit"
        case .fullAccess: "full_access"
        }
        UserDefaults.standard.set(value, forKey: key(draft))
    }
}

/// A header button: an icon with an optional label, filled while its mode is on.
struct HeaderButton: View {
    let symbol: String
    let title: String?
    var tint: Color = Theme.secondary
    var selected = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HeaderLabel(symbol: symbol, title: title, tint: tint, selected: selected)
        }
        .buttonStyle(.plain)
    }
}

/// What every header button and menu shows: an icon and an optional label on the chat
/// bubble's fill, 32 points high.
struct HeaderLabel: View {
    let symbol: String
    let title: String?
    var tint: Color = Theme.secondary
    var selected = false

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: symbol)
            if let title { Text(title).lineLimit(1) }
        }
        .font(.subheadline.weight(.semibold))
        .foregroundStyle(selected ? Theme.onPrimary : tint)
        .padding(.horizontal, title == nil ? 0 : 11)
        .frame(minWidth: 32, minHeight: 32, maxHeight: 32)
        .background(selected ? Theme.primary : Theme.bubble, in: .rect(cornerRadius: 9))
        .hitTarget()
    }
}

/// How a session's inspector shows: a pane beside the chat where there is room for both, else
/// a sheet over it, as a 300 point pane leaves a compact-width chat no room.
enum InspectorPresentation: Equatable {
    case pane, sheet

    init(compact: Bool) {
        self = compact ? .sheet : .pane
    }
}
