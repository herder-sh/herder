import Herder
import SwiftUI

/// A session: its transcript live, what it asks pinned above the composer, and the controls
/// to prompt, interrupt, answer and switch.
struct SessionView: View {
    let fleet: Fleet
    let key: SessionKey
    /// Opens another session (a child); `nil` pushes it.
    var open: ((SessionKey) -> Void)?
    @State private var switching = false
    @State private var showsTerminal = false
    @State private var showsPRs = false
    @AppStorage("listHidden") private var listHidden = false
    @AppStorage("inspectorShown") private var inspectorShown = false
    @State private var linking = false
    @State private var typedPR = ""

    var body: some View {
        let model = fleet.sessions[key]
        let summary = fleet.lists.projects.lazy.flatMap(\.sessions).first { $0.key == key }
        let blocks = model.map(Transcript.blocks) ?? []
        VStack(spacing: 0) {
            header(model, summary)
            Rectangle().fill(Theme.stroke).frame(height: 1)
            if showsTerminal {
                TerminalPane(fleet: fleet, key: key)
            } else {
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
                .frame(maxWidth: 760)
                .frame(maxWidth: .infinity)
                .padding(16)
            }
            .defaultScrollAnchor(.bottom)
            .modifier(FollowsGrowth())
            if let model {
                controls(model, summary)
            }
            }
        }
        .background(Theme.background)
        .overlay(alignment: .trailing) { EmptyView() }
        .safeAreaInset(edge: .trailing, spacing: 0) {
            if inspectorShown {
                HStack(spacing: 0) {
                    Rectangle().fill(Theme.stroke).frame(width: 1)
                    SessionInspector(fleet: fleet, key: key).frame(width: 300)
                }
            }
        }
        .onChange(of: key) { showsTerminal = false }
        .sheet(isPresented: $switching) { SwitchSheet(fleet: fleet, key: key) }
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

    private func header(_ model: SessionModel?, _ summary: SessionSummary?) -> some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 6) {
                Text(summary?.title ?? model?.title ?? "Session")
                    .font(.title3.weight(.bold)).foregroundStyle(Theme.text).lineLimit(2)
                HStack(spacing: 6) {
                    StatusGlyph(state: model?.state ?? .idle, size: 7)
                    Text(model?.state.label ?? "")
                        .foregroundStyle(model?.state == .needsYou ? Theme.accent : Theme.secondary)
                    if let summary {
                        Text("· \(summary.project.isEmpty ? "" : summary.project + " · ")\(summary.machine)")
                            .foregroundStyle(Theme.tertiary)
                    }
                }
                .font(.footnote.weight(.medium))
                if let branch = model?.branch {
                    Text(branch).font(Theme.monoSmall).foregroundStyle(Theme.tertiary).textSelection(.enabled)
                }
            }
            Spacer()
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
                HeaderButton(symbol: showsTerminal ? "text.bubble" : "terminal", title: showsTerminal ? "Chat" : "Terminal",
                             selected: showsTerminal) { showsTerminal.toggle() }
                    .keyboardShortcut("`", modifiers: .command)
                    .help(showsTerminal ? "Back to the chat (⌘`)" : "A shell in this session's worktree (⌘`)")
            }
            HeaderButton(symbol: "info.circle", title: nil, selected: inspectorShown) { inspectorShown.toggle() }
                .keyboardShortcut("i", modifiers: [.command, .option])
                .help("Events and statistics (⌥⌘I)")
            #if os(macOS)
            HeaderButton(symbol: listHidden ? "sidebar.squares.left" : "rectangle.expand.vertical",
                         title: nil) { listHidden.toggle() }
                .keyboardShortcut("\\", modifiers: .command)
                .help(listHidden ? "Show the session list (⌘\\)" : "Give the session the whole width (⌘\\)")
            #endif
            if let model, model.state != .archived {
                Menu {
                    if model.turn != nil {
                        Button("Interrupt", systemImage: "stop.circle") { Task { await fleet.interrupt(key) } }
                    }
                    Button("Switch Account or Model…", systemImage: "arrow.left.arrow.right") { switching = true }
                    Button("Link Pull Request…", systemImage: "link") { linking = true }
                    Divider()
                    Button("Archive", systemImage: "archivebox") { Task { await fleet.archive(key) } }
                } label: {
                    Image(systemName: "ellipsis")
                        .font(.callout.weight(.semibold))
                        .foregroundStyle(Theme.secondary)
                        .frame(width: 30, height: 30)
                        .background(Theme.raised, in: .rect(cornerRadius: 8))
                }
                .menuStyle(.button)
                .buttonStyle(.plain)
                .menuIndicator(.hidden)
                .fixedSize()
            }
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
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
                Composer(fleet: fleet, key: key, model: model, switching: $switching)
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
        case .archived: return "Archived · read-only"
        case .moved: return "Moved to another host · read-only here"
        default: return summary?.machineOffline == true ? "\(summary?.machine ?? "The host") is offline · read-only" : nil
        }
    }
}

/// Keeps the transcript at its end as it grows, where the OS supports it.
private struct FollowsGrowth: ViewModifier {
    func body(content: Content) -> some View {
        if #available(iOS 18, macOS 15, *) {
            content.defaultScrollAnchor(.bottom, for: .sizeChanges)
        } else {
            content
        }
    }
}

/// The prompt box of a session: model and permissions in the box, where it runs and on which
/// account under it; with a question pending it answers it.
private struct Composer: View {
    let fleet: Fleet
    let key: SessionKey
    let model: SessionModel
    @Binding var switching: Bool
    @State private var text = ""

    var body: some View {
        let machine = fleet.machines.first { $0.hostId == key.hostId }
        let accounts = machine?.accounts ?? []
        let account = accounts.first { $0.accountId == model.accountId }
        VStack(alignment: .leading, spacing: 6) {
            ComposerBox(
                text: $text,
                placeholder: placeholder,
                provider: model.provider,
                model: model.model ?? "",
                usedModels: fleet.models(on: key.hostId, provider: model.provider),
                providers: Array(Set(accounts.map(\.provider))).sorted(),
                mode: model.mode,
                running: model.turn != nil,
                setModel: { name in
                    guard let account else { return }
                    Task { await fleet.switchSession(key, to: account, model: name) }
                },
                setProvider: { provider in
                    guard provider != model.provider,
                          let target = fleet.defaultAccount(on: key.hostId, projectId: nil, provider: provider) else { return }
                    Task { await fleet.switchSession(key, to: target, model: "") }
                },
                setMode: { mode in Task { await fleet.setMode(mode, of: key) } },
                send: send,
                stop: { Task { await fleet.interrupt(key) } }
            ) {
                Label(machine?.name ?? "", systemImage: "desktopcomputer")
                FooterItem(symbol: "person.crop.circle", text: account?.label ?? model.accountId ?? "") {
                    ForEach(accounts.filter { $0.provider == model.provider }, id: \.accountId) { other in
                        Button(other.label) { Task { await fleet.switchSession(key, to: other, model: "") } }
                    }
                }
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

    private var placeholder: String {
        if !model.questions.isEmpty { return "Type an answer…" }
        return model.turn != nil ? "Queue a follow-up…" : "Ask for changes or send a follow-up"
    }

    private func send() {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        self.text = ""
        Task { await fleet.submit(text, to: key) }
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
    @State private var provider: Provider = ""
    @State private var model = ""
    @State private var mode: PermissionMode = .fullAccess
    @State private var text = ""
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
                placeholder: "Ask for changes, or describe what to build",
                provider: provider,
                model: model,
                usedModels: fleet.models(on: hostId, provider: provider),
                offersDefault: true,
                providers: Array(Set(machine?.accounts.map(\.provider) ?? [])).sorted(),
                mode: mode,
                running: false,
                setModel: { model = $0 },
                setProvider: { provider = $0; model = ModelCatalog.defaultModel($0) },
                setMode: { mode = $0 },
                send: { Task { await start() } },
                stop: {}
            ) {
                FooterItem(symbol: "desktopcomputer", text: machine?.name ?? "") {
                    ForEach(machines, id: \.hostId) { other in Button(other.name) { hostId = other.hostId } }
                }
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
            Spacer()
        }
        .padding(.horizontal, 24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
        .onAppear {
            hostId = draft.hostId
            provider = fleet.defaultProvider(on: hostId, projectId: draft.projectId) ?? ""
            model = ModelCatalog.defaultModel(provider)
            mode = ModePreference.mode(for: draft)
        }
    }

    private func start() async {
        let prompt = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !prompt.isEmpty else { return }
        guard let account = fleet.defaultAccount(on: hostId, projectId: draft.projectId, provider: provider) else {
            error = "\(machine?.name ?? "This machine") has no \(provider) account."
            return
        }
        ModePreference.remember(mode, for: draft)
        starting = prompt
        defer { starting = nil }
        do {
            created(try await fleet.createSession(
                on: hostId, repo: draft.repo, projectId: draft.projectId, accountId: account.accountId,
                model: model, mode: mode, prompt: prompt))
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
            HStack(spacing: 6) {
                Image(systemName: symbol)
                if let title { Text(title).lineLimit(1) }
            }
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(selected ? Theme.onPrimary : tint)
            .padding(.horizontal, title == nil ? 0 : 10)
            .frame(minWidth: 30, minHeight: 30)
            .background(selected ? Theme.primary : Theme.raised, in: .rect(cornerRadius: 8))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }
}
