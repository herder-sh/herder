import Herder
import SwiftUI

/// A session: its transcript live, what it asks pinned above the composer, and the controls
/// to prompt, interrupt, answer and switch.
struct SessionView: View {
    let fleet: Fleet
    let key: SessionKey
    /// Opens another session (a child); `nil` pushes it.
    var open: ((SessionKey) -> Void)?
    @State private var pushedFork: SessionKey?
    @State private var switching = false
    @State private var showsTerminal = false
    @State private var showsPRs = false
    @Environment(\.sessionPath) private var path
    @State private var showsMachineSettings = false
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
    /// The block at the top of the transcript, for the checkpoint rail.
    @State private var top = TranscriptTop()
    /// Bumped on every send, so the transcript jumps to its end.
    @State private var sent = 0
    @State private var find = TranscriptFind()

    var body: some View {
        let model = fleet.sessions[key]
        let summary = fleet.lists.projects.lazy.flatMap(\.sessions).first { $0.key == key }
        let blocks = model.map(Transcript.blocks) ?? []
        let matches = find.shown ? find.matches(blocks) : []
        VStack(spacing: 0) {
            if let parent = model?.parent {
                ChildBanner(fleet: fleet, parent: SessionKey(hostId: key.hostId, sessionId: parent), open: open)
            }
            // On a phone the navigation bar carries the title and the actions, so the
            // transcript starts right under it.
            if !compact {
                header(model, summary)
                    .overlay(alignment: .leading) {
                        if model?.parent != nil { Rectangle().fill(Theme.child).frame(width: 3) }
                    }
                Rectangle().fill(Theme.stroke).frame(height: 1)
            }
            if showsTerminal {
                TerminalPane(fleet: fleet, key: key)
            } else {
            ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 16) {
                    if model?.loaded != true && blocks.isEmpty {
                        ProgressView().tint(Theme.secondary).frame(maxWidth: .infinity).padding(40)
                    } else if blocks.isEmpty && model?.status != .waitingForCapacity {
                        Text("No turns yet. Send a prompt to start.").foregroundStyle(Theme.tertiary)
                            .frame(maxWidth: .infinity).padding(40)
                    }
                    ForEach(blocks) { block in
                        TranscriptBlockView(block: block, fleet: fleet, key: key, open: open)
                            .environment(\.findHighlight, find.highlight(block.id))
                    }
                    if model?.loaded == true && model?.status == .waitingForCapacity {
                        let resources = fleet.machines.first { $0.hostId == key.hostId }?.resources
                        WaitingForCapacityNote(load: resources.map(TurnLoad.init)) { showsMachineSettings = true }
                    }
                }
                .scrollTargetLayout()
                .frame(maxWidth: 760)
                .frame(maxWidth: .infinity)
                .padding(16)
                .overlay(alignment: .bottom) { Color.clear.frame(height: 1).id(TranscriptScroll.end) }
            }
            .defaultScrollAnchor(.bottom)
            .scrollDismissesKeyboard(.interactively)
            .accessibilityIdentifier("transcript")
            .modifier(FollowsGrowth(key: key, scroll: $scroll, top: top))
            .id(key)
            // To the last row, unanimated: a lazy stack lays that row out to scroll to it, while
            // the end marker sits where estimated heights put it, which in a long transcript can
            // be past every row drawn: blank until something scrolls it. What the send adds
            // then stays in view by the bottom anchor.
            .onChange(of: sent) { proxy.scrollTo(blocks.last?.id ?? TranscriptScroll.end, anchor: .bottom) }
            .task(id: model?.loaded == true) {
                guard model?.loaded == true else { return }
                // Once laid out, land where it was left, else at the end. The bottom anchor alone
                // lands where the lazy rows' estimated heights put the end, which in a long
                // transcript can be past every row drawn: blank until something scrolls it.
                await Task.yield()
                if let top = scroll.land() {
                    proxy.scrollTo(top, anchor: .top)
                } else {
                    proxy.scrollTo(TranscriptScroll.end, anchor: .bottom)
                }
            }
            .onChange(of: find.current) { if let id = find.current { withAnimation { proxy.scrollTo(id, anchor: .center) } } }
            .overlay(alignment: .topTrailing) {
                if find.shown {
                    FindBar(find: $find, matches: matches).padding(12)
                }
            }
            .overlay(alignment: .leading) {
                let checkpoints = Checkpoints(blocks)
                // Hover is how the rail reads; a phone has none, and no margin to spare.
                if !compact && checkpoints.items.count > 1 {
                    CheckpointRail(checkpoints: checkpoints, top: top) { id in
                        withAnimation { proxy.scrollTo(id, anchor: .top) }
                    }
                }
            }
            }
            if let model {
                controls(model, summary)
            }
            }
        }
        .environment(\.prLinks, PRLinks(linkablePRs))
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
            find = TranscriptFind()
            scroll.show(key)
            fleet.unwatch(old)
            fleet.watch(key)
        }
        .onAppear { fleet.watch(key) }
        .onDisappear { fleet.unwatch(key) }
        .navigationDestination(isPresented: Binding(
            get: { pushedFork != nil }, set: { if !$0 { pushedFork = nil } }
        )) {
            if let key = pushedFork { SessionView(fleet: fleet, key: key) }
        }
        .sheet(isPresented: $switching) { SwitchSheet(fleet: fleet, key: key) }
        .sheet(isPresented: $showsMachineSettings) { MachineSettingsSheet(fleet: fleet, hostId: key.hostId) }
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
        .toolbar {
            if compact {
                ToolbarItem(placement: .principal) { compactTitle(model, summary) }
                ToolbarItemGroup(placement: .topBarTrailing) {
                    prButton(labels: false, inToolbar: true)
                    if let model { actionsMenu(model, inToolbar: true) }
                }
            }
        }
        #endif
    }

    // MARK: Phone header

    /// The title on a phone: one line of title, tail-truncated, over one caption line of the
    /// provider, the state, the project and the machine, so it fits beside the back button.
    private func compactTitle(_ model: SessionModel?, _ summary: SessionSummary?) -> some View {
        VStack(spacing: 1) {
            HStack(spacing: 5) {
                if model?.parent != nil { ChildAvatar(session: model, size: 16, showsState: false) }
                Text(summary?.title ?? model?.title ?? "Session")
                    .font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    .lineLimit(1).truncationMode(.tail)
            }
            HStack(spacing: 4) {
                if let provider = model?.provider { ProviderMark(provider: provider, size: 10) }
                StatusGlyph(state: model?.state ?? .idle, size: 6)
                let detail = SessionHeading.detail(project: summary?.project, machine: summary?.machine,
                                                   archiving: fleet.archiving.contains(key))
                (Text(model?.state.label ?? "")
                    .foregroundStyle(model?.state == .needsYou ? Theme.accent : Theme.secondary)
                 + Text(detail.isEmpty ? "" : " · " + detail).foregroundStyle(Theme.tertiary))
                    .lineLimit(1).truncationMode(.tail)
            }
            .font(.caption2.weight(.medium))
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
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
            prButton(labels: labels, inToolbar: false)
            if ownsTerminal, model?.state != .archived {
                HeaderButton(symbol: showsTerminal ? "text.bubble" : "terminal",
                             title: labels ? showsTerminal ? "Chat" : "Terminal" : nil,
                             selected: showsTerminal) { showsTerminal.toggle() }
                    .accessibilityLabel(showsTerminal ? "Chat" : "Terminal")
                    .keyboardShortcut("`", modifiers: .command)
                    .help(showsTerminal ? "Back to the chat (⌘`)" : "A shell in this session's worktree (⌘`)")
            }
            if !showsTerminal {
                HeaderButton(symbol: "magnifyingglass", title: nil, selected: find.shown) { toggleFind() }
                    .accessibilityLabel("Find in Transcript")
                    .keyboardShortcut("f", modifiers: .command)
                    .help("Find in the transcript (⌘F)")
            }
            HeaderButton(symbol: "info.circle", title: nil, selected: inspector == .pane && inspectorShown) {
                showInspector()
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
                actionsMenu(model, inToolbar: false)
            }
            }
            // The buttons keep their size; the title, status and branch take what is left.
            .fixedSize()
        }
    }

    private var ownsTerminal: Bool {
        fleet.machines.first(where: { $0.hostId == key.hostId })?.role == .owner
    }

    private func toggleFind() { find.shown.toggle() }

    /// The pull requests `#123` in the transcript can name: the session tree's, then the
    /// project's, whose repository the rest are in.
    private var linkablePRs: [PullRequest] {
        let tree = PRRollup(of: key, sessions: fleet.sessions).groups.flatMap(\.prs)
        let project = fleet.lists.projects.first { $0.sessions.contains { $0.key == key } }?
            .sessions.flatMap(\.prs) ?? []
        return tree + project
    }

    private func showInspector() {
        switch inspector {
        case .pane: inspectorShown.toggle()
        case .sheet: inspectorSheet = true
        }
    }

    /// The session's pull requests, tinted by the most urgent, opening their list.
    @ViewBuilder
    private func prButton(labels: Bool, inToolbar: Bool) -> some View {
        let rollup = PRRollup(of: key, sessions: fleet.sessions)
        if !rollup.groups.isEmpty {
            Group {
                if inToolbar {
                    Button(rollup.chip, systemImage: "arrow.triangle.pull") { showsPRs.toggle() }
                        .tint(rollup.urgent?.color ?? Theme.secondary)
                } else {
                    HeaderButton(symbol: "arrow.triangle.pull", title: labels ? rollup.chip : rollup.shortChip,
                                 tint: rollup.urgent?.color ?? Theme.secondary) {
                        showsPRs.toggle()
                    }
                }
            }
            .accessibilityLabel(rollup.chip)
            // On compact width the popover adapts to a sheet, which PRStrip then frames.
            .popover(isPresented: $showsPRs, arrowEdge: .bottom) {
                PRStrip(fleet: fleet, key: key, rollup: rollup, presentation: PRListPresentation(compact: compact)) { child in
                    showsPRs = false
                    if let open { open(child) } else { path?.wrappedValue.append(.session(child)) }
                }
                .presentationCompactAdaptation(.sheet)
            }
        }
    }

    /// The session's actions. In the phone's toolbar it also holds what the wide header shows
    /// as buttons and lines: the terminal, the inspector, the branch and the fork origin.
    private func actionsMenu(_ model: SessionModel, inToolbar: Bool) -> some View {
        Menu {
            if inToolbar {
                // The branch and fork origin title the section, as the menu has no other line
                // for them.
                Section(SessionHeading.facts(branch: model.branch,
                                             forkedFrom: fleet.forkOrigins[key])) {
                    if ownsTerminal, model.state != .archived {
                        Button(showsTerminal ? "Chat" : "Terminal",
                               systemImage: showsTerminal ? "text.bubble" : "terminal") { showsTerminal.toggle() }
                    }
                    Button("Events and Statistics", systemImage: "info.circle") { showInspector() }
                    if !showsTerminal {
                        Button("Find in Transcript", systemImage: "magnifyingglass") { toggleFind() }
                    }
                }
            }
            if model.state != .archived {
                if model.turn != nil {
                    Button("Interrupt", systemImage: "stop.circle") { Task { await fleet.interrupt(key) } }
                }
                Button("Switch Account or Model…", systemImage: "arrow.left.arrow.right") { switching = true }
                Button("Link Pull Request…", systemImage: "link") { linking = true }
                if model.state.renamable {
                    Button("Rename…", systemImage: "pencil") { fleet.renaming = key }
                }
                Divider()
                Button("Archive", systemImage: "archivebox") { Task { await fleet.archive(key) } }
                    .disabled(fleet.archiving.contains(key))
            }
        } label: {
            if inToolbar {
                Label("Session Actions", systemImage: "ellipsis")
            } else {
                HeaderLabel(symbol: "ellipsis", title: nil)
            }
        }
        .modifier(HeaderMenuStyle(applies: !inToolbar))
    }

    // MARK: Controls

    @ViewBuilder
    private func controls(_ model: SessionModel, _ summary: SessionSummary?) -> some View {
        VStack(spacing: 8) {
            if let request = pinned(model, summary) {
                // The composer takes the user's own answer, as it would with the card gone; each
                // answer brings in the next question.
                RequestCard(request: request.request, fleet: fleet, showsSession: false, more: request.more,
                            step: request.step, answersInComposer: readOnly(model, summary) == nil)
                    .id(request.request.requestId)
                    .transition(.asymmetric(insertion: .move(edge: .trailing).combined(with: .opacity),
                                            removal: .opacity))
            }
            if let readOnly = readOnly(model, summary) {
                Label(readOnly, systemImage: "lock")
                    .font(.footnote.weight(.medium))
                    .foregroundStyle(summary?.machineOffline == true ? Theme.failure : Theme.secondary)
                    .frame(maxWidth: .infinity, minHeight: 44)
                    .background(Theme.surface, in: .rect(cornerRadius: Theme.corner))
            } else {
                Composer(fleet: fleet, key: key, model: model, sent: { sent += 1 }) { host, account in
                    Task {
                        guard let fork = await fleet.handOff(key, to: host, account: account) else { return }
                        if let open { open(fork) } else { pushedFork = fork }
                    }
                }
                // Its own box per session: switching sessions keeps what each had typed.
                .id(key)
            }
        }
        .animation(.smooth(duration: 0.25), value: pinned(model, summary)?.request.requestId)
        .frame(maxWidth: 784)
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 12)
        .padding(.top, 8)
        .padding(.bottom, 10)
        .background(Theme.background)
    }

    /// The oldest approval, else the oldest question, on any route, as the TUI pins them.
    private func pinned(_ model: SessionModel, _ summary: SessionSummary?)
        -> (request: PendingRequest, more: Int, step: (number: Int, of: Int)?)? {
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
                model.approvals.count + model.questions.count - 1,
                model.approvals.isEmpty && model.questionRun > 1
                    ? (model.questionRun - model.questions.count + 1, model.questionRun) : nil)
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
    let top: TranscriptTop

    func body(content: Content) -> some View {
        if #available(iOS 18, macOS 15, *) {
            content
                .defaultScrollAnchor(.bottom, for: .sizeChanges)
                .onScrollTargetVisibilityChange(idType: String.self, threshold: 0.01) {
                    scroll.saw($0.first, in: key)
                    top.id = $0.first
                }
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
    /// Hands the session off to a machine, on an account or the machine's default.
    let handOff: (HostId, AccountId?) -> Void
    @State private var text = ""
    @State private var images: [Herder.Image] = []
    @State private var files: [PromptFile] = []

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
            let queue = model.waiting(in: fleet.queue(of: key))
            if !queue.isEmpty {
                QueueTray(fleet: fleet, key: key, queue: queue, running: model.turn != nil)
            }
            ComposerBox(
                text: $text,
                images: $images,
                files: $files,
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
                skills: fleet.skills(of: key),
                setMode: { mode in Task { await fleet.setMode(mode, of: key) } },
                send: send,
                stop: { Task { await fleet.interrupt(key) } }
            ) {
                FooterMenu(section: machineSection, text: handoffText ?? machine?.name ?? "Machine",
                           help: "Where it runs; pick another machine to hand it off there",
                           busy: handoffText != nil)
                FooterMenu(section: accountSection(accounts), text: account?.label ?? model.accountId ?? "")
                Spacer()
                if let branch = model.branch {
                    // The header shows it too, so it gives way first.
                    Label(branch, systemImage: "arrow.triangle.branch").lineLimit(1).layoutPriority(-1)
                }
            }
            if let refusal = fleet.refusals[key] {
                Text(refusal).font(.footnote).foregroundStyle(Theme.failure).padding(.horizontal, 18)
            }
        }
        .onAppear {
            if let kept = PromptDrafts.shared.load(PromptDrafts.key(key)) {
                text = kept.text
                images = kept.herderImages
                files = kept.promptFiles
            }
        }
        // Kept a moment after the last keystroke, and at once on leaving.
        .task(id: draftContent) {
            guard (try? await Task.sleep(for: .milliseconds(300))) != nil else { return }
            PromptDrafts.shared.save(draftContent, for: PromptDrafts.key(key))
        }
        .onDisappear {
            PromptDrafts.shared.save(draftContent, for: PromptDrafts.key(key))
        }
    }

    private var draftContent: PromptDrafts.Content {
        PromptDrafts.Content(text: text, images: images, files: files)
    }

    /// The provider's accounts on the machine; picking one moves the session to it.
    private func accountSection(_ accounts: [Account]) -> SettingsSection {
        SettingsSection(
            kind: .account,
            options: SettingsOption.accounts(accounts.filter { $0.provider == model.provider }, current: model.accountId)
        ) { id in
            guard let other = accounts.first(where: { $0.accountId == id }) else { return }
            Task { await fleet.switchSession(key, to: other, model: "") }
        }
    }

    /// The machines; picking another hands the session off to it.
    private var machineSection: SettingsSection {
        fleet.machineSection(for: key, provider: model.provider, forkable: forkable, handOff: handOff)
    }

    /// "Handing off to …" while the session is being handed off.
    private var handoffText: String? {
        guard let target = fleet.handoffs[key] else { return nil }
        if target == key.hostId { return "Forking…" }
        return "Handing off to \(fleet.machines.first { $0.hostId == target }?.name ?? target)…"
    }

    /// A child session stays with its parent; a session forks once it has loaded.
    private var forkable: Bool { model.loaded && model.parent == nil }

    private var placeholder: String {
        if case .question(_, let choices) = model.questions.first?.kind {
            return choices.isEmpty ? "Type an answer…" : "Type your own answer…"
        }
        if model.parent != nil {
            return model.turn != nil ? "Queue a message for this child session…" : "Message this child session…"
        }
        return model.turn != nil ? "Queue a follow-up…" : "Ask for changes or send a follow-up"
    }

    private func send() {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let (images, files) = (images, files)
        guard !text.isEmpty || !images.isEmpty || !files.isEmpty else { return }
        self.text = ""
        self.images = []
        self.files = []
        PromptDrafts.shared.save(PromptDrafts.Content(text: "", images: []), for: PromptDrafts.key(key))
        sent()
        if model.state == .archived {
            Task { await fleet.unarchiveAndSubmit(text, images: images, files: files, to: key) }
        } else {
            Task { await fleet.submit(text, images: images, files: files, to: key) }
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
    /// Moves the draft to another project, picked from its heading.
    let moved: (Draft) -> Void
    /// Where the prompt goes as it is typed, for the draft's card in the list beside it.
    var typed: Binding<String>?
    @State private var hostId: HostId = ""
    /// The provider and model it starts on; picking another provider's model switches to it.
    @State private var choice = ModelCatalog.Choice(provider: "", model: "")
    /// The account picked in the menu; while it is not the provider's on this machine, the
    /// default account starts the session.
    @State private var accountId: AccountId?
    @State private var mode: PermissionMode = .fullAccess
    @State private var text = ""
    @State private var images: [Herder.Image] = []
    @State private var files: [PromptFile] = []
    @State private var error: String?
    /// The first message while the session is being created.
    @State private var starting: String?
    @State private var picking = false
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    /// A phone's width: the chat sits closer to the edges and the footer says less.
    private var compact: Bool { sizeClass == .compact }
    #else
    private let compact = false
    #endif

    private var machine: Machine? { fleet.machines.first { $0.hostId == hostId } }
    /// Machines the draft can run on: the connected ones with the project, or any for a path.
    private var machines: [Machine] {
        fleet.machines.filter { machine in
            machine.connection == .connected && machine.hosts.isEmpty
                && (draft.projectId == nil || machine.projects.contains { $0.projectId == draft.projectId })
        }
    }
    /// The account the session starts on.
    private var account: Account? {
        machine?.accounts.first { $0.accountId == accountId && $0.provider == choice.provider }
            ?? fleet.defaultAccount(on: hostId, projectId: draft.projectId, provider: choice.provider)
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
                .frame(maxWidth: .infinity, alignment: .trailing)
                .transition(.asymmetric(
                    insertion: .offset(y: 40).combined(with: .opacity).animation(.smooth(duration: 0.35).delay(0.25)),
                    removal: .opacity
                ))
            } else {
                heading.transition(.opacity)
            }
            // Always present, so on send it slides down to where the session's composer sits.
            ComposerBox(
                text: $text,
                images: $images,
                files: $files,
                placeholder: "Ask for changes, or describe what to build",
                models: fleet.modelGroups(on: hostId, providers: fleet.providers(on: hostId), current: choice,
                                          offersDefault: true),
                current: choice,
                mode: mode,
                running: false,
                choose: { choice = $0 },
                settings: [accountSection, machineSection],
                skills: fleet.draftSkills(on: hostId, account: account?.accountId),
                setMode: { mode = $0 },
                send: { Task { await start() } },
                stop: {}
            ) {
                FooterMenu(section: machineSection, text: machine?.name ?? "", help: "Where it runs")
                FooterMenu(section: accountSection, text: account?.label ?? "No account",
                           help: "The account it signs in with")
                Label("New worktree", systemImage: "folder.badge.plus").lineLimit(1).fixedSize()
                Spacer()
                // Where it branches from goes without saying on a phone, which has no room for it.
                if !compact {
                    Label("From the default branch", systemImage: "arrow.triangle.branch").lineLimit(1)
                }
            }
            .frame(maxWidth: 760)
            .disabled(starting != nil)
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
            }
            if starting == nil {
                Spacer()
            }
        }
        .padding(.horizontal, compact ? 12 : 24)
        .padding(.bottom, starting == nil ? 0 : 10)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
        .onAppear {
            hostId = draft.hostId
            choice = fleet.draftChoice(on: hostId, projectId: draft.projectId)
            mode = fleet.machines.first { $0.hostId == draft.hostId }?.projects
                .first { $0.projectId == draft.projectId }?.defaultPermissionMode ?? ModePreference.mode(for: draft)
            if let kept = PromptDrafts.shared.load(draft.key) {
                text = kept.text
                images = kept.herderImages
                files = kept.promptFiles
            }
        }
        // Kept a moment after the last keystroke, and at once on leaving; not while the
        // session is starting, so a sent prompt is not kept again.
        .task(id: draftContent) {
            guard starting == nil, (try? await Task.sleep(for: .milliseconds(300))) != nil else { return }
            PromptDrafts.shared.save(draftContent, for: draft.key)
        }
        .onChange(of: text, initial: true) { typed?.wrappedValue = text }
        .onDisappear {
            if starting == nil { PromptDrafts.shared.save(draftContent, for: draft.key) }
        }
        .sheet(isPresented: $picking) {
            ProjectPicker(fleet: fleet, newProject: false, picked: move(to:))
        }
    }

    /// What it asks, with where it runs as a button that picks another project.
    private var heading: some View {
        let font = Font.system(size: compact ? 24 : 30, weight: .medium)
        let project = Button { picking = true } label: {
            HStack(spacing: 8) {
                ProjectIcon(projectId: draft.projectId ?? draft.repo, name: place,
                            image: fleet.projectIcon(draft.projectId), size: compact ? 24 : 30)
                Text("\(machine?.name ?? "")/\(place)").lineLimit(1).truncationMode(.middle)
                SwiftUI.Image(systemName: "chevron.down").font(.system(size: compact ? 13 : 15, weight: .bold))
                    .foregroundStyle(Theme.secondary)
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .help("Start in another project")
        .accessibilityIdentifier("draft-project")
        return ViewThatFits(in: .horizontal) {
            HStack(spacing: 10) {
                Text("What should we build in")
                HStack(spacing: 2) { project; Text("?") }
            }
            VStack(spacing: 6) {
                Text("What should we build in")
                HStack(spacing: 2) { project; Text("?") }
            }
        }
        .font(font)
        .foregroundStyle(Theme.text)
    }

    /// Moves to the picked project, taking the prompt along; the draft left behind is forgotten.
    private func move(to picked: Draft) {
        guard picked.key != draft.key else { return }
        PromptDrafts.shared.move(PromptDrafts.Content(text: text, images: images, draft: picked), from: draft.key, to: picked.key)
        // Emptied, so leaving this draft does not keep the prompt here again.
        text = ""
        images = []
        moved(picked)
    }

    private var draftContent: PromptDrafts.Content {
        PromptDrafts.Content(text: text, images: images, files: files, draft: target)
    }

    /// The draft on the machine picked, for its card in the lists.
    private var target: Draft { Draft(hostId: hostId, projectId: draft.projectId, repo: draft.repo) }

    /// Every account on the machine, of each provider; picking another provider's account
    /// switches to that provider's default model.
    private var accountSection: SettingsSection {
        let accounts = machine?.accounts ?? []
        return SettingsSection(kind: .account, options: SettingsOption.accounts(accounts, current: account?.accountId)) { id in
            guard let picked = accounts.first(where: { $0.accountId == id }) else { return }
            accountId = id
            if picked.provider != choice.provider {
                choice = .init(provider: picked.provider, model: ModelCatalog.defaultModel(picked.provider))
            }
        }
    }

    /// The machines it can start on; picking one moves the draft there.
    private var machineSection: SettingsSection {
        SettingsSection(kind: .machine, options: SettingsOption.machines(machines, current: hostId) { _ in nil }) { id in
            hostId = id
            if let projectId = draft.projectId { MachinePreference.remember(id, for: projectId) }
            choice = fleet.draftChoice(choice, movedTo: id, projectId: draft.projectId)
        }
    }

    private func start() async {
        let prompt = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !prompt.isEmpty || !images.isEmpty || !files.isEmpty else { return }
        guard let account else {
            error = "\(machine?.name ?? "This machine") has no \(ModelCatalog.providerName(choice.provider)) account."
            return
        }
        ModePreference.remember(mode, for: draft)
        if let projectId = draft.projectId { MachinePreference.remember(hostId, for: projectId) }
        let (sent, sentFiles) = (images, files)
        withAnimation(.smooth(duration: 0.4)) {
            starting = prompt
            error = nil
            text = ""
            images = []
            files = []
        }
        PromptDrafts.shared.save(PromptDrafts.Content(text: "", images: []), for: draft.key)
        do {
            created(try await fleet.createSession(
                on: hostId, repo: draft.createArguments.repo, projectId: draft.createArguments.projectId, accountId: account.accountId,
                model: choice.model, mode: mode, prompt: prompt, images: sent, files: sentFiles))
        } catch {
            withAnimation(.smooth(duration: 0.4)) {
                starting = nil
                text = prompt
                images = sent
                files = sentFiles
                self.error = describe(error)
            }
            PromptDrafts.shared.save(PromptDrafts.Content(text: prompt, images: sent, files: sentFiles, draft: target),
                                     for: draft.key)
        }
    }
}

/// The machine last picked for a new session in a project, or last started one in it on,
/// remembered per project on this device, so one project's machine does not carry over to
/// another; its new sessions start there while it has the project.
enum MachinePreference {
    private static func key(_ projectId: String) -> String { "machine." + projectId }

    static func last(for projectId: String) -> HostId? {
        UserDefaults.standard.string(forKey: key(projectId))
    }

    static func remember(_ hostId: HostId, for projectId: String) {
        UserDefaults.standard.set(hostId, forKey: key(projectId))
    }
}

/// The permission mode new sessions of a project start in, remembered on this device; full
/// access until changed.
enum ModePreference {
    private static func key(_ draft: Draft) -> String { "mode." + draft.key }

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

/// The wide header's menu look: a plain button with no indicator, at its own size. A toolbar
/// menu keeps the system's.
private struct HeaderMenuStyle: ViewModifier {
    let applies: Bool

    func body(content: Content) -> some View {
        if applies {
            content
                .menuStyle(.button)
                .buttonStyle(.plain)
                .menuIndicator(.hidden)
                .fixedSize()
        } else {
            content
        }
    }
}

/// The phone header's caption after the session's state: its project and machine, or that it
/// is being archived.
enum SessionHeading {
    static func detail(project: String?, machine: String?, archiving: Bool) -> String {
        let parts = archiving ? ["Archiving…"] : [project, machine].compactMap { $0 }
        return parts.filter { !$0.isEmpty }.joined(separator: " · ")
    }

    /// What the phone's actions menu is titled with: the branch, and where it was forked from.
    static func facts(branch: String?, forkedFrom: ForkOrigin?) -> String {
        [branch, forkedFrom.map { "Forked from \($0.sessionId) on \($0.hostId)" }]
            .compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: "\n")
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
