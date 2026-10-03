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

    var body: some View {
        let model = fleet.sessions[key]
        let summary = fleet.lists.projects.lazy.flatMap(\.sessions).first { $0.key == key }
        let blocks = model.map(Transcript.blocks) ?? []
        VStack(spacing: 0) {
            header(model, summary)
            Rectangle().fill(Theme.stroke).frame(height: 1)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 16) {
                    if model?.loaded != true {
                        ProgressView().tint(Theme.secondary).frame(maxWidth: .infinity).padding(40)
                    } else if blocks.isEmpty {
                        Text("No turns yet. Send a prompt to start.").foregroundStyle(Theme.tertiary)
                            .frame(maxWidth: .infinity).padding(40)
                    }
                    ForEach(blocks) { block in
                        TranscriptBlockView(block: block, fleet: fleet, hostId: key.hostId, open: open)
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
        .background(Theme.background)
        .sheet(isPresented: $switching) { SwitchSheet(fleet: fleet, key: key) }
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
            if let model, model.state != .archived {
                Menu {
                    if model.turn != nil {
                        Button("Interrupt", systemImage: "stop.circle") { Task { await fleet.interrupt(key) } }
                    }
                    Button("Switch Account or Model…", systemImage: "arrow.left.arrow.right") { switching = true }
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

/// The prompt input: model, account and mode up front; send, or stop while a turn runs.
private struct Composer: View {
    let fleet: Fleet
    let key: SessionKey
    let model: SessionModel
    @Binding var switching: Bool
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        let machine = fleet.machines.first { $0.hostId == key.hostId }
        let account = machine?.accounts.first { $0.accountId == model.accountId }
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Button { switching = true } label: {
                    Chip(symbol: "sparkle", text: "\(model.provider ?? "") · \(model.model ?? "")")
                }
                .buttonStyle(.plain)
                Button { switching = true } label: {
                    Chip(symbol: "person.crop.circle", text: account?.label ?? model.accountId ?? "")
                }
                .buttonStyle(.plain)
                Menu {
                    ForEach([PermissionMode.readOnly, .ask, .autoEdit, .fullAccess], id: \.self) { mode in
                        Button {
                            Task { await fleet.setMode(mode, of: key) }
                        } label: {
                            if mode == model.mode { Label(mode.label, systemImage: "checkmark") } else { Text(mode.label) }
                        }
                    }
                } label: {
                    Chip(symbol: "lock.open", text: model.mode?.label ?? "")
                }
                .menuStyle(.button)
                .buttonStyle(.plain)
                .menuIndicator(.hidden)
                .fixedSize()
                Spacer()
            }
            HStack(alignment: .bottom, spacing: 8) {
                TextField(placeholder, text: $text, axis: .vertical)
                    .textFieldStyle(.plain)
                    .lineLimit(1...8)
                    .focused($focused)
                    .foregroundStyle(Theme.text)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 12)
                    .frame(minHeight: 44)
                    .background(Theme.raised, in: .rect(cornerRadius: 22))
                    .onSubmit(send)
                    .accessibilityIdentifier("composer")
                if model.turn != nil && trimmed.isEmpty {
                    RoundButton(symbol: "stop.fill", help: "Interrupt") { Task { await fleet.interrupt(key) } }
                } else {
                    RoundButton(symbol: "arrow.up", help: "Send", action: send)
                        .disabled(trimmed.isEmpty)
                        .opacity(trimmed.isEmpty ? 0.4 : 1)
                        .keyboardShortcut(.return, modifiers: .command)
                }
            }
            if let refusal = fleet.refusals[key] {
                Text(refusal).font(.footnote).foregroundStyle(Theme.failure)
            }
        }
    }

    private var trimmed: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    private var placeholder: String {
        if !model.questions.isEmpty { return "Type an answer…" }
        return model.turn != nil ? "Queue a message…" : "Message…"
    }

    private func send() {
        let text = trimmed
        guard !text.isEmpty else { return }
        self.text = ""
        Task { await fleet.submit(text, to: key) }
    }
}

/// A round 44-point button in the primary colour.
private struct RoundButton: View {
    let symbol: String
    let help: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.body.weight(.bold))
                .foregroundStyle(Theme.onPrimary)
                .frame(width: 44, height: 44)
                .background(Theme.primary, in: .circle)
                .contentShape(.circle)
        }
        .buttonStyle(.plain)
        .help(help)
        .accessibilityLabel(help)
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

/// A new session's empty chat: where it runs, with its provider, model and permissions
/// preselected as chips; the first message creates the session.
struct DraftSessionView: View {
    let fleet: Fleet
    let draft: Draft
    let created: (SessionKey) -> Void
    @State private var provider: Provider = ""
    @State private var model = ""
    @State private var mode: PermissionMode = .fullAccess
    @State private var text = ""
    @State private var editingModel = false
    @State private var error: String?
    @FocusState private var focused: Bool

    private var machine: Machine? { fleet.machines.first { $0.hostId == draft.hostId } }
    private var project: Project? { machine?.projects.first { $0.projectId == draft.projectId } }
    private var place: String { project?.name ?? draft.repo.map { URL(fileURLWithPath: $0).lastPathComponent } ?? "" }

    var body: some View {
        let providers = Array(Set(machine?.accounts.map(\.provider) ?? [])).sorted()
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 6) {
                Text("New session").font(.title3.weight(.bold)).foregroundStyle(Theme.text)
                Text("\(place) · \(machine?.name ?? "")").font(.footnote.weight(.medium)).foregroundStyle(Theme.tertiary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 20)
            .padding(.vertical, 14)
            Rectangle().fill(Theme.stroke).frame(height: 1)
            VStack(spacing: 10) {
                Image(systemName: "text.bubble").font(.largeTitle).foregroundStyle(Theme.tertiary)
                Text("What should the agent do in \(place)?").font(.headline).foregroundStyle(Theme.secondary)
                Text("It starts on a fresh worktree and branch.").font(.footnote).foregroundStyle(Theme.tertiary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 6) {
                    if providers.count > 1 {
                        Menu {
                            ForEach(providers, id: \.self) { name in Button(name) { provider = name } }
                        } label: { Chip(symbol: "sparkle", text: provider) }
                            .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
                    }
                    Button { editingModel = true } label: {
                        Chip(symbol: "cpu", text: model.isEmpty ? "Default model" : model)
                    }
                    .buttonStyle(.plain)
                    Menu {
                        ForEach([PermissionMode.readOnly, .ask, .autoEdit, .fullAccess], id: \.self) { option in
                            Button(option.label) { mode = option }
                        }
                    } label: { Chip(symbol: "lock.open", text: mode.label) }
                        .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize()
                    Spacer()
                }
                HStack(alignment: .bottom, spacing: 8) {
                    TextField("Message…", text: $text, axis: .vertical)
                        .textFieldStyle(.plain)
                        .lineLimit(1...8)
                        .foregroundStyle(Theme.text)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 12)
                        .frame(minHeight: 44)
                        .background(Theme.raised, in: .rect(cornerRadius: 22))
                        .focused($focused)
                        .onSubmit { Task { await start() } }
                        .accessibilityIdentifier("composer")
                    RoundButton(symbol: "arrow.up", help: "Start") { Task { await start() } }
                        .disabled(trimmed.isEmpty)
                        .opacity(trimmed.isEmpty ? 0.4 : 1)
                }
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Theme.failure)
                }
            }
            .frame(maxWidth: 784)
            .frame(maxWidth: .infinity)
            .padding(.horizontal, 12)
            .padding(.top, 8)
            .padding(.bottom, 10)
        }
        .background(Theme.background)
        .alert("Model", isPresented: $editingModel) {
            TextField("Provider's default", text: $model)
            Button("Done") {}
        } message: {
            Text("A model in the provider's naming, or blank for its default.")
        }
        .onAppear {
            provider = fleet.defaultAccount(on: draft.hostId, projectId: draft.projectId, provider: nil)?.provider
                ?? providers.first ?? ""
            mode = ModePreference.mode(for: draft)
            focused = true
        }
    }

    private var trimmed: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    private func start() async {
        guard !trimmed.isEmpty,
              let account = fleet.defaultAccount(on: draft.hostId, projectId: draft.projectId, provider: provider)
        else {
            error = "This machine has no account for \(provider)."
            return
        }
        ModePreference.remember(mode, for: draft)
        do {
            created(try await fleet.createSession(
                on: draft.hostId, repo: draft.repo, projectId: draft.projectId, accountId: account.accountId,
                model: model.trimmingCharacters(in: .whitespaces), mode: mode, prompt: trimmed))
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
