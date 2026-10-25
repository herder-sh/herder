import Herder
import SwiftUI

/// An account to add, as the form has it. Until the user types an id, it follows the first
/// free one for the provider.
struct AccountDraft {
    var id = ""
    var provider: Provider = "claude"
    var label = ""
    var configDir = ""
    /// Whether the user typed the id; until then `suggest` keeps it free.
    var idEdited = false

    /// Sets the id to the first free one for the provider, unless the user typed one.
    mutating func suggest(taken: [String]) {
        if !idEdited { id = Self.nextId(provider: provider, taken: taken) }
    }

    /// The first id not in `taken`: the provider's name, or with the login's email known, the
    /// provider and the email's name (`claude-sam`); then the same numbered from 2.
    static func nextId(provider: Provider, taken: [String], email: String? = nil) -> String {
        let name = email.flatMap { $0.split(separator: "@", omittingEmptySubsequences: false).first }
            .map { String($0).lowercased().map { $0.isLetter || $0.isNumber || $0 == "." ? $0 : "-" } }
            .map { String($0).trimmingCharacters(in: CharacterSet(charactersIn: ".-")) }
        let base = name.flatMap { $0.isEmpty ? nil : "\(provider)-\($0)" } ?? provider
        if !taken.contains(base) { return base }
        var n = 2
        while taken.contains("\(base)-\(n)") { n += 1 }
        return "\(base)-\(n)"
    }

    /// The ids a new account on `machine` cannot take: its accounts, the logins it reports
    /// running (from any device), and `pending`, the logins this device started there.
    static func taken(on machine: Machine?, pending: [AccountId]) -> [String] {
        let running = machine?.terminals.compactMap { terminal -> AccountId? in
            if case .login(let id) = terminal.purpose { return id }
            return nil
        } ?? []
        return Array(Set((machine?.accounts.map(\.accountId) ?? []) + running + pending)).sorted()
    }

    /// The label shown until the user types one: the provider's name, numbered as the id is.
    var suggestedLabel: String {
        let id = account.accountId
        let name = ModelCatalog.providerName(provider)
        if id == provider { return name }
        if id.hasPrefix(provider + "-") { return "\(name) \(id.dropFirst(provider.count + 1))" }
        return id
    }

    var account: NewAccount {
        NewAccount(accountId: id.trimmingCharacters(in: .whitespacesAndNewlines), provider: provider,
                   label: clean(label), configDir: clean(configDir))
    }

    /// What the daemon is asked to add: the label typed, else the suggested one.
    var submitted: NewAccount {
        var account = account
        account.label = account.label ?? suggestedLabel
        return account
    }

    func problem(existing: [String]) -> String? {
        let value = account
        guard !value.accountId.isEmpty else { return "Enter an account ID." }
        guard value.accountId.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0)
            || (48...57).contains($0) || [45, 95, 46].contains($0) }) else {
            return "Account IDs use letters, numbers, hyphens, underscores and dots."
        }
        if existing.contains(value.accountId) {
            return "\(value.accountId) is taken on this machine, by an account or a login running."
        }
        if let path = value.configDir, !Self.validPath(path) {
            return "Use an absolute path or a path starting with ~/."
        }
        return nil
    }

    static func validPath(_ path: String) -> Bool {
        !path.contains("\0") && (path.hasPrefix("/") || path.hasPrefix("~/"))
    }

    private func clean(_ text: String) -> String? {
        let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }
}

/// Which page the add-account sheet shows.
enum AddAccountPage: Equatable {
    /// The form for a new account.
    case form
    /// The logins this device started on the machine, to go back to one or add another.
    case logins
    /// One login or install, in its terminal.
    case job(ProviderJobKey)

    /// `shown` is the terminal the user opened, `adds` the logins on the machine, and
    /// `addingAnother` whether the user asked for the form while they run.
    static func resolve(shown: ProviderJobKey?, exists: Bool, adds: [ProviderJobKey], addingAnother: Bool) -> Self {
        if let shown, exists { return .job(shown) }
        return adds.isEmpty || addingAnother ? .form : .logins
    }
}

/// Adds an account on a machine: the form, then the provider's own login in a terminal. Logins
/// run side by side, for any providers; reopened while some run, the sheet lists them to go
/// back to, with a way to add another.
struct AddAccountSheet: View {
    let fleet: Fleet
    let hostId: HostId
    /// The provider a fresh form starts on.
    var initialProvider: Provider = "claude"
    /// The account to start from, as another machine has it.
    var initialDraft: AccountDraft?
    @State private var draft = AccountDraft()
    @State private var started = false
    @State private var shown: ProviderJobKey?
    @State private var addingAnother = false
    @State private var showsAdvanced = false

    private var machine: Machine? { fleet.machines.first { $0.hostId == hostId } }
    private var adds: [ProviderJobKey] { fleet.providerTerminals.adds(on: hostId) }
    private var taken: [String] {
        AccountDraft.taken(on: machine, pending: fleet.providerTerminals.pendingIds(on: hostId))
    }
    private var problem: String? { draft.problem(existing: taken) }
    private var canManage: Bool { machine?.role == .owner && machine?.connection == .connected }
    private var status: ProviderStatus? { machine?.providers.first { $0.provider == draft.provider } }
    /// Whether the machine reports the provider's CLI missing: its login cannot run.
    private var missing: Bool { status?.installed == false }
    private var page: AddAccountPage {
        AddAccountPage.resolve(shown: shown, exists: shown.map { fleet.providerTerminals[$0] != nil } ?? false,
                               adds: adds, addingAnother: addingAnother)
    }

    var body: some View {
        let page = page
        SheetScaffold(title: title(page), subtitle: machine?.name ?? "Machine", height: 680) {
            switch page {
            case .form: form
            case .logins: logins
            case .job(let key): JobTerminal(fleet: fleet, key: key)
            }
        } footer: {
            switch page {
            case .form: formFooter
            case .logins:
                ActionButton(title: "Add Another Account", style: .secondary) { freshForm() }
                    .frame(maxWidth: 220)
                Spacer()
            case .job(let key): jobFooter(key)
            }
        }
        .onAppear {
            guard !started else { return }
            started = true
            if let initialDraft {
                draft = initialDraft
                addingAnother = true
            } else {
                draft = AccountDraft(provider: initialProvider)
                draft.suggest(taken: taken)
            }
        }
        .onChange(of: draft.provider) { draft.suggest(taken: taken) }
        .onChange(of: taken) { draft.suggest(taken: taken) }
    }

    private func title(_ page: AddAccountPage) -> String {
        if case .job(let key) = page, case .install(let provider) = key.job {
            return "Install \(ModelCatalog.providerName(provider))"
        }
        return "Add Account"
    }

    // MARK: Form

    @ViewBuilder private var form: some View {
        Field(label: "Provider") {
            ChoiceChips(options: [("claude", "Claude", ""), ("codex", "Codex", ""),
                                  ("cursor", "Cursor", ""), ("opencode", "OpenCode", "")],
                        selection: $draft.provider)
        }
        if missing { install }
        Field(label: "Account ID", hint: "Unique on \(machine?.name ?? "this machine"); sessions and failover name the account by it.") {
            InputBox(placeholder: draft.provider, text: Binding(get: { draft.id }, set: { draft.id = $0; draft.idEdited = true }),
                     mono: true)
        }
        Field(label: "Display label") { InputBox(placeholder: draft.suggestedLabel, text: $draft.label) }
        DisclosureGroup(isExpanded: $showsAdvanced) {
            InputBox(placeholder: "~/.\(draft.provider)-\(draft.account.accountId)", text: $draft.configDir, mono: true)
                .padding(.top, 8)
            Text(configDirHint).font(.footnote).foregroundStyle(Theme.tertiary).padding(.top, 4)
        } label: {
            SectionHeading(title: "Config directory")
        }
        if !draft.id.isEmpty, let problem {
            Label(problem, systemImage: "exclamationmark.circle").font(.footnote).foregroundStyle(Theme.failure)
        }
        if !canManage {
            Text("Connect as the machine owner to add an account.").font(.footnote).foregroundStyle(Theme.secondary)
        }
    }

    private var configDirHint: String {
        let defaultLogin = draft.provider == "claude" ? " ~/.claude adds Claude’s default login." : ""
        return "Optional: a new directory, or one already logged in.\(defaultLogin) Leave it empty to let herder choose."
    }

    /// The provider's CLI is missing on the machine: install it first, where herder can.
    private var install: some View {
        let name = ModelCatalog.providerName(draft.provider)
        return HStack(alignment: .center, spacing: 12) {
            Image(systemName: "shippingbox").foregroundStyle(Theme.accent)
            VStack(alignment: .leading, spacing: 2) {
                Text("\(name) isn’t installed on \(machine?.name ?? "this machine")")
                    .font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                Text(status?.canInstall == true ? "Its login needs the CLI. herder runs the vendor’s installer, in view."
                     : "herder has no installer for it on this machine’s OS; install it there, then add the account.")
                    .font(.caption).foregroundStyle(Theme.secondary).fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            if status?.canInstall == true {
                Button("Install") { shown = fleet.install(draft.provider, on: hostId) }
                    .disabled(!canManage)
            }
        }
        .padding(12)
        .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
    }

    @ViewBuilder private var formFooter: some View {
        if !adds.isEmpty {
            ActionButton(title: adds.count == 1 ? "Show Login" : "Show \(adds.count) Logins", style: .secondary) {
                addingAnother = false
            }
            .frame(maxWidth: 180)
        }
        Spacer()
        ActionButton(title: "Start Login", style: .primary) {
            guard canManage, problem == nil, !missing else { return }
            shown = fleet.providerTerminals.add(draft.submitted, on: hostId)
        }
        .frame(maxWidth: 180)
        .disabled(!canManage || problem != nil || missing)
    }

    // MARK: Logins

    /// The logins this device started on the machine, newest first.
    @ViewBuilder private var logins: some View {
        Text("Logins you started on \(machine?.name ?? "this machine"). They keep running while this sheet is closed.")
            .font(.footnote).foregroundStyle(Theme.secondary)
        VStack(spacing: 0) {
            ForEach(Array(adds.reversed().enumerated()), id: \.element) { index, key in
                if index > 0 { RowDivider() }
                if let connection = fleet.providerTerminals[key], let account = connection.account {
                    let progress = JobProgress(connection.state, added: added(account.accountId))
                    HStack(spacing: 10) {
                        Button { shown = key } label: {
                            HStack(spacing: 12) {
                                ProviderMark(provider: account.provider, size: 18).frame(width: 22)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(account.label ?? account.accountId).font(.subheadline.weight(.medium))
                                        .foregroundStyle(Theme.text)
                                    Text("\(ModelCatalog.providerName(account.provider)) · \(account.accountId)")
                                        .font(.caption).foregroundStyle(Theme.tertiary)
                                }
                                Spacer(minLength: 8)
                                JobBadge(progress: progress)
                            }
                            .frame(minHeight: 52)
                            .contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("\(ModelCatalog.providerName(account.provider)) \(account.accountId), \(progress.label)")
                        .accessibilityIdentifier("running-login-\(account.accountId)")
                        if !progress.finished { CancelLoginButton(fleet: fleet, key: key, compact: true) }
                        Button { shown = key } label: {
                            Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                                .frame(width: 20, height: 44).contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                        .accessibilityHidden(true)
                    }
                    .padding(.horizontal, 14)
                }
            }
        }
        .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
    }

    private func added(_ accountId: AccountId) -> Bool {
        machine?.accounts.contains { $0.accountId == accountId } == true
    }

    // MARK: Terminal

    @ViewBuilder private func jobFooter(_ key: ProviderJobKey) -> some View {
        let progress = fleet.providerTerminals[key].map { JobProgress($0.state, added: jobAdded(key)) } ?? .ended
        if progress.finished {
            Spacer()
            ActionButton(title: "Done", style: .primary) {
                fleet.providerTerminals.clear(key)
                if case .install = key.job {
                    shown = nil
                } else {
                    freshForm()
                }
            }
            .frame(maxWidth: 180)
        } else {
            if case .add = key.job {
                ActionButton(title: "Add Another Account", style: .secondary) { freshForm() }.frame(maxWidth: 220)
                CancelLoginButton(fleet: fleet, key: key, compact: false).frame(maxWidth: 160)
            }
            Spacer()
            Text("Keeps running if you close this").font(.footnote).foregroundStyle(Theme.tertiary)
        }
    }

    private func jobAdded(_ key: ProviderJobKey) -> Bool {
        if case .add(let id) = key.job { return added(id) }
        return false
    }

    /// A fresh form, while any logins run on.
    private func freshForm() {
        shown = nil
        addingAnother = true
        draft = AccountDraft(provider: draft.provider)
        draft.suggest(taken: taken)
    }
}

/// A login or install in its terminal, under a line saying what it is and where it stands.
struct JobTerminal: View {
    let fleet: Fleet
    let key: ProviderJobKey

    private var machine: Machine? { fleet.machines.first { $0.hostId == key.hostId } }

    var body: some View {
        if let connection = fleet.providerTerminals[key] {
            let progress = if case .install = key.job { fleet.installProgress(key) }
                else { JobProgress(connection.state, added: added) }
            HStack(spacing: 10) {
                ProviderMark(provider: provider(connection), size: 18)
                VStack(alignment: .leading, spacing: 2) {
                    Text(heading(connection)).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    Text(guidance(progress)).font(.caption).foregroundStyle(Theme.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 8)
                JobBadge(progress: progress)
            }
            TerminalSurface(connection: connection, client: fleet.client, sessionId: nil)
                .frame(height: 390)
                .background(.black, in: .rect(cornerRadius: Theme.corner))
                .clipShape(.rect(cornerRadius: Theme.corner))
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("login-terminal")
            if progress == .added {
                Label("Account added", systemImage: "checkmark.circle.fill").foregroundStyle(Theme.success)
            }
        }
    }

    private var added: Bool {
        guard case .add(let id) = key.job else { return false }
        return machine?.accounts.contains { $0.accountId == id } == true
    }

    private func provider(_ connection: TerminalConnection) -> Provider {
        if let account = connection.account { return account.provider }
        if let install = connection.install { return install }
        return machine?.accounts.first { $0.accountId == connection.relogin }?.provider ?? ""
    }

    private func heading(_ connection: TerminalConnection) -> String {
        switch key.job {
        case .add(let id):
            "\(connection.account?.label ?? id) · \(id)"
        case .relogin(let id): "Logging \(id) in again"
        case .install(let provider):
            fleet.providerTerminals.before(key) != nil
                ? "Updating \(ModelCatalog.providerName(provider))" : "Installing \(ModelCatalog.providerName(provider))"
        }
    }

    private func guidance(_ progress: JobProgress) -> String {
        switch (key.job, progress) {
        case (.install, .running), (.install, .starting): "The vendor’s installer runs below, on \(machine?.name ?? "the machine")."
        case (.install, .failed(let message)): message
        case (.install, _): "\(progress.label) on \(machine?.name ?? "the machine")."
        case (_, .added): "The machine lists the account; sessions can use it now."
        case (_, .ended): "The login ended without the provider reporting the account logged in."
        case (_, .failed(let message)): message
        default: "Finish the provider’s own login below, on \(machine?.name ?? "the machine")."
        }
    }
}

/// Cancels a running login, once asked to confirm: closes its terminal on the machine, which
/// hangs up the provider's login, so it adds no account. `compact` for a row, else a footer's.
struct CancelLoginButton: View {
    let fleet: Fleet
    let key: ProviderJobKey
    let compact: Bool
    @State private var confirming = false

    private var connection: TerminalConnection? { fleet.providerTerminals[key] }

    private var subject: String {
        switch key.job {
        case .add(let id), .relogin(let id): id
        case .install(let provider): ModelCatalog.providerName(provider)
        }
    }

    var body: some View {
        Group {
            if compact {
                PillButton(title: "Cancel") { confirming = true }
            } else {
                ActionButton(title: "Cancel Login", style: .secondary) { confirming = true }
            }
        }
        // The terminal has an id once the machine opened it.
        .disabled(connection?.terminalId == nil)
        .accessibilityLabel("Cancel Login \(subject)")
        .confirmationDialog("Cancel the login of \(subject)?", isPresented: $confirming, titleVisibility: .visible) {
            Button("Cancel Login", role: .destructive) {
                Task { await cancel() }
            }
            Button("Keep Running", role: .cancel) {}
        } message: {
            Text(key.job == .relogin(subject) ? "The account keeps its current login."
                 : "The machine closes the login’s terminal and adds no account.")
        }
    }

    private func cancel() async {
        do {
            try await fleet.cancelLogin(key)
        } catch {
            fleet.toast = .failure("Could not cancel the login of \(subject)", error)
        }
    }
}

/// A login's or install's progress as a small coloured label.
struct JobBadge: View {
    let progress: JobProgress

    var body: some View {
        HStack(spacing: 5) {
            if progress == .starting || progress == .running {
                ProgressView().controlSize(.mini)
            } else {
                Circle().fill(progress.tint).frame(width: 6, height: 6)
            }
            Text(progress.label)
        }
        .font(.caption.weight(.medium))
        .foregroundStyle(progress.tint)
        .lineLimit(1)
        .fixedSize()
    }
}

extension JobProgress {
    var label: String {
        switch self {
        case .starting: "Starting"
        case .running: "Waiting for you"
        case .added: "Added"
        case .ended: "Ended"
        case .installed(let version): version.isEmpty ? "Installed" : "Installed \(version)"
        case .updated(let from, let to): from.isEmpty || to.isEmpty ? "Updated" : "Updated \(from) → \(to)"
        case .upToDate(let version): version.isEmpty ? "Up to date" : "Up to date \(version)"
        case .failed: "Failed"
        }
    }

    var tint: Color {
        switch self {
        case .starting, .running: Theme.accent
        case .added, .installed, .updated, .upToDate: Theme.success
        case .ended: Theme.tertiary
        case .failed: Theme.failure
        }
    }
}

struct AccountTarget: Equatable, Identifiable {
    let hostId: HostId
    let name: String
    /// Why an account cannot be added there from this device; `nil` when it can.
    let problem: String?

    var id: HostId { hostId }

    /// The machines that run sessions: a vault runs none, and a machine never connected has
    /// not said whether this device owns it. Adding an account is the owner's, on a connected
    /// machine.
    static func all(_ machines: [Machine]) -> [AccountTarget] {
        machines.filter { $0.hosts.isEmpty && $0.role != nil }.map { machine in
            AccountTarget(hostId: machine.hostId, name: machine.name,
                          problem: machine.role != .owner ? "Owners only"
                              : machine.connection != .connected ? "Not connected" : nil)
        }
    }

    /// The machine to add on without asking: the only one where this device can.
    static func only(_ targets: [AccountTarget]) -> HostId? {
        let usable = targets.filter { $0.problem == nil }
        return usable.count == 1 ? usable[0].hostId : nil
    }
}

/// Adds a provider account from anywhere: picks the machine, then runs the add-account sheet's
/// login there.
struct NewAccountSheet: View {
    let fleet: Fleet
    var provider: Provider = "claude"
    @State private var picked: HostId?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let targets = AccountTarget.all(fleet.machines)
        if let hostId = picked ?? AccountTarget.only(targets) {
            AddAccountSheet(fleet: fleet, hostId: hostId, initialProvider: provider)
        } else {
            SheetScaffold(title: "Add Account", subtitle: "Pick the machine to log in on", height: 460) {
                if targets.isEmpty {
                    Text("Pair a machine first; accounts live on the machines that run sessions.")
                        .foregroundStyle(Theme.secondary)
                }
                VStack(spacing: 0) {
                    ForEach(Array(targets.enumerated()), id: \.element.id) { index, target in
                        if index > 0 { RowDivider() }
                        Button { picked = target.hostId } label: {
                            HStack(spacing: 10) {
                                Image(systemName: "desktopcomputer").foregroundStyle(Theme.secondary)
                                Text(target.name).foregroundStyle(Theme.text)
                                Spacer(minLength: 8)
                                let running = fleet.providerTerminals.pendingIds(on: target.hostId).count
                                if running > 0 {
                                    Text(running == 1 ? "1 login running" : "\(running) logins running")
                                        .font(.caption).foregroundStyle(Theme.accent)
                                }
                                if let problem = target.problem {
                                    Text(problem).font(.caption).foregroundStyle(Theme.tertiary)
                                } else {
                                    Image(systemName: "chevron.right").font(.caption.weight(.semibold))
                                        .foregroundStyle(Theme.tertiary)
                                }
                            }
                            .font(.subheadline)
                            .padding(.horizontal, 14)
                            .frame(minHeight: 44)
                            .contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                        .disabled(target.problem != nil)
                    }
                }
                .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
                .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
                Text("The provider’s own login runs on that machine, under the new account’s config directory.")
                    .font(.footnote).foregroundStyle(Theme.tertiary)
            } footer: {
                Spacer()
                ActionButton(title: "Cancel", style: .secondary) { dismiss() }.frame(maxWidth: 180)
            }
        }
    }
}

struct EditAccountSheet: View {
    let fleet: Fleet
    let hostId: HostId
    let account: Account
    @State private var label = ""
    @State private var configDir = ""
    @State private var fallback = false
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    private var pathProblem: String? {
        let path = configDir.trimmingCharacters(in: .whitespacesAndNewlines)
        return path.isEmpty || AccountDraft.validPath(path) ? nil : "Use an absolute path or a path starting with ~/."
    }

    private var canManage: Bool {
        fleet.machines.contains { $0.hostId == hostId && $0.role == .owner && $0.connection == .connected }
    }

    private var key: ProviderJobKey { ProviderJobKey(hostId: hostId, job: .relogin(account.accountId)) }

    /// This account's login, when one runs again from here.
    private var login: TerminalConnection? { fleet.providerTerminals[key] }

    var body: some View {
        SheetScaffold(title: login == nil ? "Account Settings" : "Log In Again",
                      subtitle: "\(ModelCatalog.providerName(account.provider)) · \(account.accountId)",
                      height: login == nil ? 560 : 680) {
            if login != nil {
                JobTerminal(fleet: fleet, key: key)
            } else {
                settings
            }
        } footer: {
            if let login {
                if ProviderTerminals.running(login) {
                    CancelLoginButton(fleet: fleet, key: key, compact: false).frame(maxWidth: 160)
                    Spacer()
                    Text("Keeps running if you close this").font(.footnote).foregroundStyle(Theme.tertiary)
                } else {
                    Spacer()
                    ActionButton(title: "Done", style: .primary) { fleet.providerTerminals.clear(key) }
                        .frame(maxWidth: 180)
                }
            } else {
                ActionButton(title: "Log In Again", style: .secondary) {
                    guard canManage else { return }
                    fleet.providerTerminals.relogin(account.accountId, on: hostId)
                }.frame(maxWidth: 180).disabled(!canManage)
                Spacer()
                save
            }
        }
        .onAppear { label = account.label; configDir = account.configDir ?? ""; fallback = account.fallback }
    }

    @ViewBuilder private var settings: some View {
        Field(label: "Display label") { InputBox(placeholder: account.accountId, text: $label) }
        Field(label: "Config directory", hint: "On this machine. Leave empty to use the provider’s default login. Archive all sessions on the machine before changing this directory.") {
            InputBox(placeholder: "Provider default", text: $configDir, mono: true)
        }
        if let pathProblem { Text(pathProblem).font(.footnote).foregroundStyle(Theme.failure) }
        Field(label: "Rotation", hint: "A fallback login is picked for new sessions and failover only once every other \(account.provider) account is at its limit or logged out. Choosing it by name still uses it.") {
            ChoiceChips(options: [(false, "Use in rotation", ""), (true, "Fallback only", "")], selection: $fallback)
        }
        if !canManage {
            Text("Connect as the machine owner to save changes.").font(.footnote).foregroundStyle(Theme.secondary)
        }
        if let error { Text(error).font(.footnote).foregroundStyle(Theme.failure) }
        Text("Log In Again runs the provider’s login in this account’s config directory, for a login that expired.")
            .font(.footnote).foregroundStyle(Theme.tertiary)
    }

    private var save: some View {
        ActionButton(title: "Save", style: .primary) {
            guard canManage, pathProblem == nil else { return }
            do {
                let path = configDir.trimmingCharacters(in: .whitespacesAndNewlines)
                _ = try await fleet.client.send(hostId: hostId, command: .setAccountSettings(
                    accountId: account.accountId, label: label.trimmingCharacters(in: .whitespacesAndNewlines),
                    configDir: path.isEmpty ? nil : path, fallback: fallback))
                dismiss()
            } catch { self.error = describe(error) }
        }.frame(maxWidth: 180).disabled(!canManage || pathProblem != nil || label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
    }
}
