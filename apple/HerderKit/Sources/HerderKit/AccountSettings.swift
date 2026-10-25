import Herder
import SwiftUI

/// An account to add, as the form has it: one login for every machine it is added on. Each
/// machine gets the first id free there, named after the label.
struct AccountDraft {
    /// The id to keep wherever it is free, as a login set up from another machine has; empty
    /// names it after the label.
    var id = ""
    var provider: Provider = "claude"
    var label = ""
    var configDir = ""

    /// The first id not in `taken`: the provider's name, or with a name for the login (an email,
    /// whose local part counts, or a label), the provider and that name (`claude-sam`); then
    /// the same numbered from 2.
    static func nextId(provider: Provider, taken: [String], name: String? = nil) -> String {
        let slug = name.flatMap { $0.split(separator: "@", omittingEmptySubsequences: false).first }
            .map { String($0).lowercased().map { $0.isLetter || $0.isNumber || $0 == "." ? $0 : "-" } }
            .map { String($0).trimmingCharacters(in: CharacterSet(charactersIn: ".-")) }
        let base = slug.flatMap { $0.isEmpty || $0 == provider ? nil : "\(provider)-\($0)" } ?? provider
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

    /// The label shown until the user types one: the provider's name, numbered as `id` is.
    static func suggestedLabel(id: AccountId, provider: Provider) -> String {
        let name = ModelCatalog.providerName(provider)
        if id == provider { return name }
        if id.hasPrefix(provider + "-") { return "\(name) \(id.dropFirst(provider.count + 1))" }
        return id
    }

    /// What a machine whose ids are `taken` is asked to add: the draft's id where it is free,
    /// else the first free one; the label typed, else the suggested one.
    func account(taken: [String]) -> NewAccount {
        let kept = id.trimmingCharacters(in: .whitespacesAndNewlines)
        let id = !kept.isEmpty && !taken.contains(kept)
            ? kept : Self.nextId(provider: provider, taken: taken, name: clean(label))
        return NewAccount(accountId: id, provider: provider,
                          label: clean(label) ?? Self.suggestedLabel(id: id, provider: provider),
                          configDir: clean(configDir))
    }

    var problem: String? {
        if let path = clean(configDir), !Self.validPath(path) {
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

extension Fleet {
    /// Starts adding `draft`'s account on each of `hostIds`: one login per machine, each in
    /// Activity, under the id free there.
    @discardableResult
    func addAccount(_ draft: AccountDraft, on hostIds: [HostId]) -> [JobKey] {
        hostIds.map { hostId in
            let taken = AccountDraft.taken(on: machines.first { $0.hostId == hostId },
                                           pending: activity.pendingIds(on: hostId))
            return addAccount(draft.account(taken: taken), on: hostId)
        }
    }
}

/// A machine an account can be added on, or why it cannot be from this device.
struct AccountTarget: Equatable, Identifiable {
    let hostId: HostId
    let name: String
    /// Why an account cannot be added there from this device; `nil` when it can.
    let problem: String?
    /// Whether the provider's CLI is missing there and herder can install it.
    var installable = false

    var id: HostId { hostId }

    /// The machines that run sessions: a vault runs none, and a machine never connected has
    /// not said whether this device owns it. Adding an account is the owner's, on a connected
    /// machine with `provider`'s CLI.
    static func all(_ machines: [Machine], provider: Provider) -> [AccountTarget] {
        machines.filter { $0.hosts.isEmpty && $0.role != nil }.map { machine in
            let status = machine.providers.first { $0.provider == provider }
            let reachable = machine.role == .owner && machine.connection == .connected
            let missing = status?.installed == false
            let problem: String? = if machine.role != .owner {
                "Member — can’t add accounts"
            } else if machine.connection != .connected {
                "Not connected"
            } else if missing {
                "\(ModelCatalog.providerName(provider)) isn’t installed"
            } else {
                nil
            }
            return AccountTarget(hostId: machine.hostId, name: machine.name, problem: problem,
                                 installable: reachable && missing && status?.canInstall == true)
        }
    }
}

/// Adds an account on several machines at once: the provider, the machines to log in on and
/// the label. Each machine's login runs in Activity, so the sheet closes once they start.
struct AddAccountSheet: View {
    let fleet: Fleet
    /// The machine ticked to start with; without one, every machine it can be added on.
    var hostId: HostId?
    /// The provider a fresh form starts on.
    var initialProvider: Provider = "claude"
    /// The account to start from, as another machine has it.
    var initialDraft: AccountDraft?
    @State private var draft = AccountDraft()
    @State private var ticked: Set<HostId> = []
    @State private var started = false
    @State private var showsConfigDir = false
    @Environment(\.dismiss) private var dismiss

    private var targets: [AccountTarget] { AccountTarget.all(fleet.machines, provider: draft.provider) }
    /// The ticked machines it can be added on now, in the list's order.
    private var chosen: [HostId] { targets.filter { $0.problem == nil && ticked.contains($0.hostId) }.map(\.hostId) }

    var body: some View {
        SheetScaffold(title: "Add Account", height: 540, hint: "Logins run in Activity; this sheet closes.") {
            AddAccountForm(draft: $draft, ticked: $ticked, showsConfigDir: $showsConfigDir, targets: targets,
                           installing: { fleet.activity.phase(JobKey(hostId: $0, kind: .install(draft.provider))) != .ended },
                           install: { fleet.install(draft.provider, on: $0) })
        } footer: {
            Spacer()
            ActionButton(title: Self.title(chosen.count), style: .primary) {
                guard !chosen.isEmpty, draft.problem == nil else { return }
                fleet.addAccount(draft, on: chosen)
                dismiss()
            }
            .fixedSize()
            .disabled(chosen.isEmpty || draft.problem != nil)
            .keyboardShortcut(.defaultAction)
        }
        .onAppear {
            guard !started else { return }
            started = true
            draft = initialDraft ?? AccountDraft(provider: initialProvider)
            ticked = hostId.map { [$0] } ?? Set(targets.filter { $0.problem == nil }.map(\.hostId))
        }
    }

    static func title(_ machines: Int) -> String {
        machines == 1 ? "Log in on 1 machine" : "Log in on \(machines) machines"
    }
}

/// The add-account form, on plain values so it can be drawn without a fleet.
struct AddAccountForm: View {
    @Binding var draft: AccountDraft
    @Binding var ticked: Set<HostId>
    @Binding var showsConfigDir: Bool
    let targets: [AccountTarget]
    let installing: (HostId) -> Bool
    let install: (HostId) -> Void

    static let providers: [Provider] = ["claude", "codex", "cursor", "opencode"]

    var body: some View {
        Field(label: "Provider") {
            FlowLayout(spacing: 8) {
                ForEach(Self.providers, id: \.self) { provider in
                    ProviderTile(provider: provider, selected: provider == draft.provider) { draft.provider = provider }
                }
            }
        }
        Field(label: "Log in on") {
            VStack(spacing: 0) {
                if targets.isEmpty {
                    Text("Pair a machine first.").font(.subheadline).foregroundStyle(Theme.secondary)
                        .padding(.horizontal, 14).frame(minHeight: 44)
                }
                ForEach(Array(targets.enumerated()), id: \.element.id) { index, target in
                    if index > 0 { RowDivider() }
                    machineRow(target)
                }
            }
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
        }
        Field(label: "Label") {
            InputBox(placeholder: ModelCatalog.providerName(draft.provider), text: $draft.label)
                .accessibilityIdentifier("account-label")
        }
        DisclosureGroup(isExpanded: $showsConfigDir) {
            InputBox(placeholder: "Chosen by herder, such as ~/.\(draft.provider)-work", text: $draft.configDir, mono: true)
                .padding(.top, 8)
        } label: {
            SectionHeading(title: "Config directory")
        }
        if let problem = draft.problem {
            Label(problem, systemImage: "exclamationmark.circle").font(.footnote).foregroundStyle(Theme.failure)
        }
    }

    private func machineRow(_ target: AccountTarget) -> some View {
        let on = target.problem == nil && ticked.contains(target.hostId)
        return HStack(spacing: 10) {
            Button {
                if ticked.contains(target.hostId) { ticked.remove(target.hostId) } else { ticked.insert(target.hostId) }
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: on ? "checkmark.square.fill" : "square")
                        .foregroundStyle(on ? Theme.text : Theme.tertiary)
                    Text(target.name).foregroundStyle(target.problem == nil ? Theme.text : Theme.tertiary)
                    if let problem = target.problem {
                        Text(problem).font(.caption).foregroundStyle(Theme.tertiary)
                    }
                    Spacer(minLength: 8)
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .disabled(target.problem != nil)
            .accessibilityIdentifier("account-machine-\(target.name)")
            if target.installable {
                if installing(target.hostId) {
                    Text("Installing…").font(.caption).foregroundStyle(Theme.secondary)
                } else {
                    PillButton(title: "Install") { install(target.hostId) }
                }
            }
        }
        .font(.subheadline)
        .padding(.horizontal, 14)
        .frame(minHeight: 44)
    }
}

/// A provider to pick, as a tile with its mark.
private struct ProviderTile: View {
    let provider: Provider
    let selected: Bool
    let pick: () -> Void

    var body: some View {
        Button(action: pick) {
            HStack(spacing: 7) {
                ProviderMark(provider: provider, size: 15)
                Text(ModelCatalog.providerName(provider)).font(.subheadline.weight(.semibold))
            }
            .foregroundStyle(Theme.text)
            .padding(.horizontal, 12)
            .frame(minHeight: 40)
            .primaryFill(in: .rect(cornerRadius: Theme.corner), on: selected)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("provider-\(provider)")
        .accessibilityAddTraits(selected ? .isSelected : [])
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
