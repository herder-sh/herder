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

/// Adds an account on a machine: the form, then the provider's own login, which runs in the
/// Activity list, so the sheet closes once it starts.
struct AddAccountSheet: View {
    let fleet: Fleet
    let hostId: HostId
    /// The provider a fresh form starts on.
    var initialProvider: Provider = "claude"
    /// The account to start from, as another machine has it.
    var initialDraft: AccountDraft?
    @State private var draft = AccountDraft()
    @State private var started = false
    @State private var showsAdvanced = false
    @Environment(\.dismiss) private var dismiss

    private var machine: Machine? { fleet.machines.first { $0.hostId == hostId } }
    private var taken: [String] {
        AccountDraft.taken(on: machine, pending: fleet.activity.pendingIds(on: hostId))
    }
    private var problem: String? { draft.problem(existing: taken) }
    private var canManage: Bool { machine?.role == .owner && machine?.connection == .connected }
    private var status: ProviderStatus? { machine?.providers.first { $0.provider == draft.provider } }
    /// Whether the machine reports the provider's CLI missing: its login cannot run.
    private var missing: Bool { status?.installed == false }
    /// Whether this provider's install runs from here on the machine.
    private var installing: Bool {
        fleet.activity.phase(JobKey(hostId: hostId, kind: .install(draft.provider))) != .ended
    }

    var body: some View {
        SheetScaffold(title: "Add Account", subtitle: machine?.name ?? "Machine", height: 600) {
            form
        } footer: {
            Spacer()
            ActionButton(title: "Start Login", style: .primary) {
                guard canManage, problem == nil, !missing else { return }
                fleet.addAccount(draft.submitted, on: hostId)
                dismiss()
            }
            .frame(maxWidth: 180)
            .disabled(!canManage || problem != nil || missing)
        }
        .onAppear {
            guard !started else { return }
            started = true
            if let initialDraft {
                draft = initialDraft
            } else {
                draft = AccountDraft(provider: initialProvider)
                draft.suggest(taken: taken)
            }
        }
        .onChange(of: draft.provider) { draft.suggest(taken: taken) }
        .onChange(of: taken) { draft.suggest(taken: taken) }
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
                Text(status?.canInstall == true ? "Its login needs the CLI. herder runs the vendor’s installer in Activity."
                     : "herder has no installer for it on this machine’s OS; install it there, then add the account.")
                    .font(.caption).foregroundStyle(Theme.secondary).fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            if installing {
                Text("Installing…").font(.caption).foregroundStyle(Theme.secondary)
            } else if status?.canInstall == true {
                Button("Install") { fleet.install(draft.provider, on: hostId) }
                    .disabled(!canManage)
            }
        }
        .padding(12)
        .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
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
                                let running = fleet.activity.pendingIds(on: target.hostId).count
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

    var body: some View {
        SheetScaffold(title: "Account Settings",
                      subtitle: "\(ModelCatalog.providerName(account.provider)) · \(account.accountId)", height: 560) {
            settings
        } footer: {
            ActionButton(title: "Log In Again", style: .secondary) {
                guard canManage else { return }
                fleet.relogin(account.accountId, on: hostId)
                dismiss()
            }.frame(maxWidth: 180).disabled(!canManage)
            Spacer()
            save
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
        Text("Log In Again runs the provider’s login in this account’s config directory, for a login that expired. It runs in Activity.")
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
