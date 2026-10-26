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
    @State private var showsTerminal = false
    @AppStorage("listHidden") private var listHidden = false
    @Environment(\.pages) private var pages
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
    /// The block at the top of the transcript, for the checkpoint rail.
    @State private var top = TranscriptTop()
    @State private var find = TranscriptFind()
    /// The folded blocks shown open, by id; outside the rows, so a row drawn again keeps them.
    @State private var expanded: Set<String> = []

    var body: some View {
        let model = fleet.session(key)
        let summary = fleet.lists.projects.lazy.flatMap(\.sessions).first { $0.key == key }
        let blocks = fleet.transcript(key)
        let matches = find.shown ? find.matches(blocks) : []
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
            // The transcript takes the height the header and the controls leave. Asked how tall
            // it would be, a scroll view lays its whole content out to answer, at each size the
            // stack tries: on every token a turn streams.
            Color.clear.overlay {
            ScrollViewReader { proxy in
            ScrollView {
                // Lazy, with each block under its own id: a long session draws only the rows in
                // view, and rows added or grown above the reader do not move what they read.
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
                    // A turn waiting for its usage limit to reset says so in its failure's card.
                    if model?.loaded == true && model?.status == .waitingForCapacity && model?.retryAt == nil {
                        let resources = fleet.machines.first { $0.hostId == key.hostId }?.resources
                        WaitingForCapacityNote(load: resources.map(TurnLoad.init)) { pages.open(.machine(key.hostId)) }
                    }
                }
                .scrollTargetLayout()
                .frame(maxWidth: SessionColumn.transcript)
                .frame(maxWidth: .infinity)
                .padding(SessionColumn.transcriptMargin)
            }
            .scrollDismissesKeyboard(.interactively)
            .accessibilityIdentifier("transcript")
            .modifier(FollowsEnd(blocks: blocks, top: top))
            .id(key)
            .environment(\.transcriptScroll) { id, anchor in proxy.scrollTo(id, anchor: anchor) }
            .onChange(of: find.current) {
                guard let id = find.current else { return }
                // A match in a turn's folded work, or in earlier reports, opens it.
                if let block = blocks.first(where: { $0.id == id }) { expanded.formUnion(find.opens(block)) }
                withAnimation { proxy.scrollTo(id, anchor: .center) }
            }
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
            }
            if let model {
                controls(model, summary)
            }
            }
        }
        .environment(\.prLinks, PRLinks(linkablePRs))
        .environment(\.transcriptExpanded, $expanded)
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
            expanded = []
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
        .sheet(isPresented: $inspectorSheet) {
            SessionInspector(fleet: fleet, key: key)
                .presentationDetents([.medium, .large])
                .presentationBackground(Theme.surface)
        }
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .toolbar {
            if compact {
                ToolbarItemGroup(placement: .topBarTrailing) {
                    prChip(inToolbar: true)
                    if let model { actionsMenu(model, threads: summary?.threads ?? [], inToolbar: true) }
                }
            }
        }
        #endif
    }

    // MARK: Header

    /// The header, between the transcript's left edge and the composer's right one: the title,
    /// the status and the branch, then a row of header buttons centred on the title's first
    /// line. On a phone the navigation bar holds the buttons, and the title gets the whole width.
    private func header(_ model: SessionModel?, _ summary: SessionSummary?) -> some View {
        SessionColumnLayout {
            if compact {
                heading(model, summary)
            } else {
                HStack(alignment: .titleLine, spacing: 16) {
                    heading(model, summary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    buttons(model, summary)
                }
            }
        }
        // 12 points over the 28 point title line centre it in `Theme.headerHeight`, on the line
        // of the list pane's title and the sidebar's top bar.
        .padding(.vertical, compact ? 4 : 12)
        .frame(minHeight: compact ? nil : Theme.headerHeight)
    }

    /// The title, at most two lines; the status line; the branch and where it was forked from.
    private func heading(_ model: SessionModel?, _ summary: SessionSummary?) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .top, spacing: 8) {
                if model?.parent != nil { ChildAvatar(session: model, size: 22, showsState: false) }
                Text(summary?.title ?? model?.title ?? "Session")
                    .font(Theme.page).foregroundStyle(Theme.text).lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
                    .titleLine()
                    .accessibilityAddTraits(.isHeader)
            }
            // One line, so a narrow header truncates where it runs on rather than wrapping
            // its parts beside each other.
            HStack(spacing: 6) {
                StatusGlyph(state: model?.state ?? .idle, size: 7)
                let detail = SessionHeading.detail(project: summary?.project, machine: summary?.machine,
                                                   archiving: fleet.archiving.contains(key))
                if model?.status == .needsLogin {
                    NeedsSignInChip()
                    if !detail.isEmpty { Text(detail).foregroundStyle(Theme.tertiary).lineLimit(1) }
                } else {
                    (Text(model?.stateLabel ?? "")
                        .foregroundStyle(model?.state == .needsYou ? Theme.accent : Theme.secondary)
                     + Text(detail.isEmpty ? "" : " · " + detail).foregroundStyle(Theme.tertiary))
                        .lineLimit(1)
                }
            }
            .font(Theme.body.weight(.medium))
            if let branch = model?.branch {
                Text(branch).font(Theme.monoSmall).foregroundStyle(Theme.tertiary).textSelection(.enabled)
                    .lineLimit(1).truncationMode(.middle)
            }
            if let origin = fleet.forkOrigins[key] {
                Text("Forked from \(origin.sessionId) on \(origin.hostId)")
                    .font(Theme.meta).foregroundStyle(Theme.tertiary).textSelection(.enabled)
                    .lineLimit(1).truncationMode(.middle)
            }
        }
    }

    /// The header's buttons, icons with their tooltips: the linked thread, the PRs, the
    /// terminal, find, the inspector, the list (on the Mac) and the session's menu. They keep
    /// their size; the title, status and branch take what is left.
    private func buttons(_ model: SessionModel?, _ summary: SessionSummary?) -> some View {
        HStack(spacing: HeaderControl.spacing) {
            if let thread = summary?.threads.last(where: { $0.url != nil }), let url = thread.url.flatMap(URL.init) {
                Link(destination: url) {
                    HeaderLabel(symbol: "bubble.left.and.text.bubble.right")
                }
                .buttonStyle(.plain)
                .help("Open the linked \(thread.kind.label) thread")
                .accessibilityLabel("Open in \(thread.kind.label)")
            }
            prChip(inToolbar: false)
            if ownsTerminal, model?.state != .archived {
                HeaderButton(symbol: showsTerminal ? "text.bubble" : "terminal", selected: showsTerminal) {
                    showsTerminal.toggle()
                }
                    .accessibilityLabel(showsTerminal ? "Chat" : "Terminal")
                    .keyboardShortcut("`", modifiers: .command)
                    .help(showsTerminal ? "Back to the chat (⌘`)" : "A shell in this session's worktree (⌘`)")
            }
            if !showsTerminal {
                HeaderButton(symbol: "magnifyingglass", selected: find.shown) { toggleFind() }
                    .accessibilityLabel("Find in Transcript")
                    .keyboardShortcut("f", modifiers: .command)
                    .help("Find in the transcript (⌘F)")
            }
            HeaderButton(symbol: "info.circle", selected: inspector == .pane && inspectorShown) {
                showInspector()
            }
                .accessibilityLabel("Events and Statistics")
                .keyboardShortcut("i", modifiers: [.command, .option])
                .help("Events and statistics (⌥⌘I)")
            #if os(macOS)
            HeaderButton(symbol: listHidden ? "sidebar.squares.left" : "rectangle.expand.vertical") {
                listHidden.toggle()
            }
                .accessibilityLabel(listHidden ? "Show the Session List" : "Hide the Session List")
                .keyboardShortcut("\\", modifiers: .command)
                .help(listHidden ? "Show the session list (⌘\\)" : "Give the session the whole width (⌘\\)")
            #endif
            if let model {
                actionsMenu(model, threads: summary?.threads ?? [], inToolbar: false)
            }
        }
        .fixedSize()
    }

    private var ownsTerminal: Bool {
        fleet.machines.first(where: { $0.hostId == key.hostId })?.role == .owner
    }

    private func toggleFind() { find.shown.toggle() }

    /// The pull requests `#123` in the transcript can name: the session tree's, then the
    /// project's, whose repository the rest are in.
    private var linkablePRs: [PullRequest] {
        let tree = PRRollup(of: key, session: fleet.session, children: fleet.lists.children).all
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

    /// The session tree's pull requests and their count, tinted by their state.
    @ViewBuilder
    private func prChip(inToolbar: Bool) -> some View {
        let rollup = PRRollup(of: key, session: fleet.session, children: fleet.lists.children)
        if !rollup.all.isEmpty {
            Group {
                if inToolbar {
                    Label(rollup.chip, systemImage: "arrow.triangle.pull")
                        .labelStyle(.titleAndIcon)
                        .font(.subheadline.weight(.semibold).monospacedDigit())
                        .foregroundStyle(rollup.urgency.color)
                } else {
                    HeaderLabel(symbol: "arrow.triangle.pull", title: rollup.chip, tint: rollup.urgency.color)
                }
            }
            .help(rollup.summary)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(rollup.summary)
        }
    }

    /// The session's actions. In the phone's toolbar it also holds what the wide header shows
    /// as buttons and lines: the terminal, the inspector, the branch and the fork origin.
    private func actionsMenu(_ model: SessionModel, threads: [ThreadLink], inToolbar: Bool) -> some View {
        Menu {
            if inToolbar {
                Section {
                    if ownsTerminal, model.state != .archived {
                        Button(showsTerminal ? "Chat" : "Terminal",
                               systemImage: showsTerminal ? "text.bubble" : "terminal") { showsTerminal.toggle() }
                    }
                    Button("Events and Statistics", systemImage: "info.circle") { showInspector() }
                    if !showsTerminal {
                        Button("Find in Transcript", systemImage: "magnifyingglass") { toggleFind() }
                    }
                    if let thread = threads.last(where: { $0.url != nil }), let url = thread.url.flatMap(URL.init) {
                        Link(destination: url) {
                            Label("Open \(thread.kind.label) Thread", systemImage: "bubble.left.and.text.bubble.right")
                        }
                    }
                }
            }
            if model.state != .archived {
                if model.turn != nil {
                    Button("Interrupt", systemImage: "stop.circle") { Task { await fleet.interrupt(key) } }
                }
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
                HeaderLabel(symbol: "ellipsis")
            }
        }
        .modifier(HeaderMenuStyle(applies: !inToolbar))
        .help("Interrupt, rename or archive")
    }

    // MARK: Controls

    @ViewBuilder
    private func controls(_ model: SessionModel, _ summary: SessionSummary?) -> some View {
        VStack(spacing: 8) {
            if let request = pinned(model, summary) {
                // The composer takes the user's own answer, as it would with the card gone; each
                // answer brings in the next question.
                RequestCard(request: request, fleet: fleet, showsSession: false,
                            answersInComposer: readOnly(model, summary) == nil)
                    .id(request.requestId)
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
                Composer(fleet: fleet, key: key, model: model) { host, account in
                    Task {
                        guard let fork = await fleet.handOff(key, to: host, account: account) else { return }
                        if let open { open(fork) } else { pushedFork = fork }
                    }
                }
                // Its own box per session: switching sessions keeps what each had typed.
                .id(key)
            }
        }
        .animation(.smooth(duration: 0.25), value: pinned(model, summary)?.requestId)
        .frame(maxWidth: SessionColumn.composer)
        .frame(maxWidth: .infinity)
        .padding(.horizontal, SessionColumn.composerMargin)
        .padding(.top, 8)
        .padding(.bottom, 10)
        .background(Theme.background)
    }

    /// The oldest approval, else the oldest question, on any route, as the TUI pins them.
    private func pinned(_ model: SessionModel, _ summary: SessionSummary?) -> PendingRequest? {
        summary.flatMap { model.pinned($0, userOnly: false) }
    }

    /// Why the session cannot be driven from here, if it cannot.
    private func readOnly(_ model: SessionModel, _ summary: SessionSummary?) -> String? {
        switch model.state {
        case .moved: return "Moved to another host · read-only here"
        default: return summary?.machineOffline == true ? "\(summary?.machine ?? "The host") is offline · read-only" : nil
        }
    }
}

/// Keeps the transcript at its end while the reader is there, following what comes, the
/// session's history included as it loads. Scrolled up, nothing moves what they read: what
/// comes below shows a "New" pill instead. Tells `top` the block at the top of the view.
private struct FollowsEnd: ViewModifier {
    let blocks: [TranscriptBlock]
    let top: TranscriptTop
    /// Not bound to a block: only the pill sets it, to the end, past the transcript's margin.
    @State private var position = ScrollPosition()
    @State private var atEnd = true
    /// The transcript grew below the reader since they scrolled up.
    @State private var unseen = false

    private func grew() {
        if !atEnd { unseen = true }
    }

    func body(content: Content) -> some View {
        content
            .scrollPosition($position)
            .defaultScrollAnchor(.bottom, for: .initialOffset)
            .defaultScrollAnchor(.bottom, for: .alignment)
            // Anchored at the bottom, growth anywhere pushes what is read up; unanchored, the
            // view keeps its offset, and a lazy stack keeps the rows in view where they are.
            .defaultScrollAnchor(atEnd ? .bottom : nil, for: .sizeChanges)
            .onScrollTargetVisibilityChange(idType: String.self, threshold: 0.01) { top.id = $0.first }
            .onScrollGeometryChange(for: Bool.self) { geometry in
                geometry.visibleRect.maxY >= geometry.contentSize.height + geometry.contentInsets.bottom - 40
            } action: { _, end in
                atEnd = end
                if end { unseen = false }
            }
            // A block more, or the last one grown: a running turn's "Working" line stays last.
            .onChange(of: blocks.count) { grew() }
            .onChange(of: blocks.last) { grew() }
            .overlay(alignment: .bottom) {
                if unseen {
                    Button {
                        withAnimation { position.scrollTo(edge: .bottom) }
                    } label: {
                        Label("New", systemImage: "arrow.down")
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(Theme.text)
                            .padding(.horizontal, 12)
                            .padding(.vertical, 6)
                            .background(Theme.surface, in: .capsule)
                            .overlay(Capsule().strokeBorder(Theme.stroke))
                            .contentShape(.capsule)
                    }
                    .buttonStyle(.plain)
                    .padding(.bottom, 12)
                    .transition(.opacity)
                    .accessibilityIdentifier("transcript-new")
                }
            }
    }
}

/// The prompt box of a session: model and permissions in the box, where it runs and on which
/// account under it; with a question pending it answers it.
private struct Composer: View {
    let fleet: Fleet
    let key: SessionKey
    let model: SessionModel
    /// Hands the session off to a machine, on an account or the machine's default.
    let handOff: (HostId, AccountId?) -> Void
    @State private var text = ""
    @State private var images: [Herder.Image] = []
    @State private var files: [PromptFile] = []

    var body: some View {
        let machine = fleet.machines.first { $0.hostId == key.hostId }
        let accounts = machine?.accounts ?? []
        let account = accounts.first { $0.accountId == model.accountId }
        // The provider is the session's; its account menu moves it to another provider's account.
        let current = ModelCatalog.Choice(provider: model.provider ?? "", model: model.model ?? "")
        VStack(alignment: .leading, spacing: 6) {
            if model.state == .archived {
                Label("Archived · sending a message brings it back", systemImage: "archivebox")
                    .font(.caption).foregroundStyle(Theme.tertiary).padding(.horizontal, 18)
            }
            // Held messages show in the transcript, above the failure that holds them.
            let queue = model.status == .needsLogin ? [] : model.waiting(in: fleet.queue(of: key))
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

    /// The machine's accounts, the session's provider's first; picking one moves the session to
    /// it, onto another provider's default model when it is another provider's.
    private func accountSection(_ accounts: [Account]) -> SettingsSection {
        let ordered = accounts.filter { $0.provider == model.provider } + accounts.filter { $0.provider != model.provider }
        return SettingsSection(
            kind: .account,
            options: SettingsOption.accounts(ordered, current: model.accountId),
            hint: ordered.contains { $0.provider != model.provider } ? "Another provider’s starts on its default model" : nil
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
        if case .question(_, let choices, _) = model.questions.first?.kind {
            return choices.isEmpty ? "Type an answer…" : "Type your own answer…"
        }
        if model.status == .needsLogin { return "Messages you send now wait with the others" }
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
        if model.state == .archived {
            Task { await fleet.unarchiveAndSubmit(text, images: images, files: files, to: key) }
        } else {
            Task { await fleet.submit(text, images: images, files: files, to: key) }
        }
    }
}

/// The amber "Needs sign-in" chip on the status line of a session whose account is signed out.
struct NeedsSignInChip: View {
    var body: some View {
        Label("Needs sign-in", systemImage: "exclamationmark.circle.fill")
            .font(.caption.weight(.semibold))
            .foregroundStyle(Theme.accent)
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(Theme.accent.opacity(0.14), in: .capsule)
            .overlay(Capsule().strokeBorder(Theme.accent.opacity(0.5)))
            .fixedSize()
            .accessibilityIdentifier("needs-sign-in")
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
    /// Machines the draft can run on: the connected ones with the project, or any for a path
    /// or a chat.
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
                placeholder: draft.isChat ? "Ask anything" : "Ask for changes, or describe what to build",
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
                // A chat works in a folder of its own, with nothing to branch from.
                if !draft.isChat {
                    Label("New worktree", systemImage: "folder.badge.plus").lineLimit(1).fixedSize()
                }
                Spacer()
                // Where it branches from goes without saying on a phone, which has no room for it.
                if !compact && !draft.isChat {
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
        // A phone shows the draft full screen; the picker pushes there rather than open over it.
        .sheet(isPresented: Binding(get: { picking && !compact }, set: { picking = $0 })) {
            ProjectPicker(fleet: fleet, newProject: false, picked: move(to:))
        }
        #if os(iOS)
        .navigationDestination(isPresented: Binding(get: { picking && compact }, set: { picking = $0 })) {
            ProjectPicker(fleet: fleet, newProject: false, picked: move(to:))
        }
        #endif
    }

    /// What it asks, with where it runs as a button that picks another project; a chat just
    /// asks, as it runs in no project.
    @ViewBuilder private var heading: some View {
        let font = Font.system(size: compact ? 24 : 30, weight: .medium)
        if draft.isChat {
            Text("Ask anything").font(font).foregroundStyle(Theme.text)
        } else {
            projectHeading(font)
        }
    }

    private func projectHeading(_ font: Font) -> some View {
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
            if draft.projectId != nil || draft.isChat { MachinePreference.remember(id, for: draft.key) }
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
        if draft.projectId != nil || draft.isChat { MachinePreference.remember(hostId, for: draft.key) }
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
                on: hostId, repo: draft.createArguments.repo, projectId: draft.createArguments.projectId,
                chat: draft.isChat, accountId: account.accountId,
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
/// remembered per project on this device, and for chats by `Draft.chatKey`, so one project's machine does not carry over to
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

/// The header's status line after the session's state: its project and machine, or that it
/// is being archived.
enum SessionHeading {
    static func detail(project: String?, machine: String?, archiving: Bool) -> String {
        let parts = archiving ? ["Archiving…"] : [project, machine].compactMap { $0 }
        return parts.filter { !$0.isEmpty }.joined(separator: " · ")
    }
}

/// The session's column: its content between the transcript's left edge and the composer's
/// right one, at every width, so the header and a child's banner line up with both.
enum SessionColumn {
    /// The transcript's widest column, and its margin in a narrower pane: a list pane's.
    static let transcript: CGFloat = 760
    static let transcriptMargin = Theme.paneMargin
    /// The composer's widest column, and its margin in a narrower pane.
    static let composer: CGFloat = 784
    static let composerMargin: CGFloat = 12

    /// How far in from a pane `width` wide the column's edges sit.
    static func insets(in width: CGFloat) -> (leading: CGFloat, trailing: CGFloat) {
        (max(transcriptMargin, (width - transcript) / 2), max(composerMargin, (width - composer) / 2))
    }
}

/// Lays its content out in the `SessionColumn` of the width it is offered.
struct SessionColumnLayout: Layout {
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard let content = subviews.first else { return .zero }
        guard let width = proposal.width, width.isFinite else {
            let ideal = content.sizeThatFits(.unspecified)
            return CGSize(width: ideal.width + SessionColumn.transcriptMargin + SessionColumn.composerMargin,
                          height: ideal.height)
        }
        let insets = SessionColumn.insets(in: width)
        let height = content.sizeThatFits(ProposedViewSize(width: width - insets.leading - insets.trailing,
                                                           height: proposal.height)).height
        return CGSize(width: width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let insets = SessionColumn.insets(in: bounds.width)
        subviews.first?.place(at: CGPoint(x: bounds.minX + insets.leading, y: bounds.minY), anchor: .topLeading,
                              proposal: ProposedViewSize(width: bounds.width - insets.leading - insets.trailing,
                                                         height: bounds.height))
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
