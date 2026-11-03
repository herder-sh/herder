import Herder
import SwiftUI

/// An account to add, as the form has it: one login, or one endpoint, for every machine it is
/// added on. Each machine gets the first id free there, named after the label.
struct AccountDraft {
    /// The id to keep wherever it is free, as a login set up from another machine has; empty
    /// names it after the label.
    var id = ""
    var provider: Provider = "claude"
    var label = ""
    var configDir = ""
    /// It runs against an API server, `url` with `key`, rather than a login; only for a provider
    /// that can (see `supportsEndpoint`).
    var endpoint = false
    var url = ""
    /// The endpoint's client key; sent once, and never shown again.
    var key = ""

    /// The providers whose CLI herder can point at an endpoint.
    static func supportsEndpoint(_ provider: Provider) -> Bool {
        provider == "claude" || provider == "codex"
    }

    /// Whether the draft adds an endpoint account: chosen, for a provider that can.
    var usesEndpoint: Bool { endpoint && Self.supportsEndpoint(provider) }

    /// An `http` or `https` URL with a host, as the daemon takes for an endpoint.
    static func validEndpoint(_ text: String) -> Bool {
        guard let url = URL(string: text.trimmingCharacters(in: .whitespacesAndNewlines)),
              let scheme = url.scheme?.lowercased(), scheme == "http" || scheme == "https" else { return false }
        return url.host?.isEmpty == false
    }

    /// Whether every field it needs is filled in and valid.
    var complete: Bool {
        guard problem == nil else { return false }
        guard usesEndpoint else { return true }
        return Self.validEndpoint(url) && clean(key) != nil
    }

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

    /// What a machine whose ids are `taken` is asked to add, as an endpoint account; `nil`
    /// unless the draft is a complete one.
    func endpointCommand(taken: [String]) -> CommandBody? {
        guard usesEndpoint, complete, let url = clean(url), let key = clean(key) else { return nil }
        let account = account(taken: taken)
        return .addEndpointAccount(accountId: account.accountId, provider: provider, label: account.label,
                                   endpoint: url, key: key)
    }

    var problem: String? {
        if usesEndpoint {
            if let url = clean(url), !Self.validEndpoint(url) { return "Use an http:// or https:// URL." }
            return nil
        }
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

    /// Adds `draft`'s endpoint account on each of `hostIds` at once, under the id free there;
    /// no terminal, as it has no login. Returns why each machine that refused it did.
    func addEndpointAccount(_ draft: AccountDraft, on hostIds: [HostId]) async -> [HostId: String] {
        let commands = hostIds.compactMap { hostId -> (HostId, CommandBody)? in
            let taken = AccountDraft.taken(on: machines.first { $0.hostId == hostId },
                                           pending: activity.pendingIds(on: hostId))
            return draft.endpointCommand(taken: taken).map { (hostId, $0) }
        }
        let client = client
        return await withTaskGroup(of: (HostId, String?).self) { group in
            for (hostId, command) in commands {
                group.addTask {
                    do {
                        _ = try await client.send(hostId: hostId, command: command)
                        return (hostId, nil)
                    } catch {
                        return (hostId, describe(error))
                    }
                }
            }
            var failures: [HostId: String] = [:]
            for await (hostId, failure) in group { failures[hostId] = failure }
            return failures
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
    /// Whether this device owns the machine: only an owner adds accounts, or sees a key field.
    var owner = true

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
                                 installable: reachable && missing && status?.canInstall == true,
                                 owner: machine.role == .owner)
        }
    }
}

/// Adds an account on several machines at once: the provider, how it runs, the machines and
/// the label. Each machine's login runs in Activity, so the sheet closes once they start; an
/// endpoint account is sent to each machine, and the sheet closes once every one took it, or
/// says why a machine refused it.
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
    @State private var sending = false
    /// Why each machine refused the endpoint account last sent.
    @State private var failures: [HostId: String] = [:]
    @Environment(\.dismiss) private var dismiss

    private var targets: [AccountTarget] { AccountTarget.all(fleet.machines, provider: draft.provider) }
    /// The ticked machines it can be added on now, in the list's order.
    private var chosen: [HostId] { targets.filter { $0.problem == nil && ticked.contains($0.hostId) }.map(\.hostId) }

    var body: some View {
        SheetScaffold(title: "Add Account", height: 600, hint: Self.hint(endpoint: draft.usesEndpoint)) {
            AddAccountForm(draft: $draft, ticked: $ticked, showsConfigDir: $showsConfigDir, targets: targets,
                           installing: { fleet.activity.phase(JobKey(hostId: $0, kind: .install(draft.provider))) != .ended },
                           install: { fleet.install(draft.provider, on: $0) }, failures: failures)
        } footer: {
            Spacer()
            ActionButton(title: sending ? "Adding…" : Self.title(chosen.count, endpoint: draft.usesEndpoint),
                         style: .primary) { add() }
            .frame(width: 220)
            .disabled(chosen.isEmpty || !draft.complete || sending)
            .keyboardShortcut(.defaultAction)
        }
        .onChange(of: draft.provider) { failures = [:] }
        .onAppear {
            guard !started else { return }
            started = true
            draft = initialDraft ?? AccountDraft(provider: initialProvider)
            ticked = hostId.map { [$0] } ?? Set(targets.filter { $0.problem == nil }.map(\.hostId))
        }
    }

    private func add() {
        guard !chosen.isEmpty, draft.complete, !sending else { return }
        guard draft.usesEndpoint else {
            fleet.addAccount(draft, on: chosen)
            dismiss()
            return
        }
        let hostIds = chosen
        sending = true
        Task {
            failures = await fleet.addEndpointAccount(draft, on: hostIds)
            sending = false
            // The machines that took it are done; those that refused stay ticked to try again.
            ticked = ticked.filter { failures[$0] != nil || !hostIds.contains($0) }
            if failures.isEmpty { dismiss() }
        }
    }

    static func title(_ machines: Int, endpoint: Bool = false) -> String {
        let verb = endpoint ? "Add" : "Log in"
        return machines == 1 ? "\(verb) on 1 machine" : "\(verb) on \(machines) machines"
    }

    static func hint(endpoint: Bool) -> String {
        endpoint ? "herder never shows the key again."
            : "Logins run in Activity; this sheet closes."
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
    /// Why each machine refused the endpoint account last sent.
    var failures: [HostId: String] = [:]

    static let providers: [Provider] = ["claude", "codex", "cursor", "opencode"]

    var body: some View {
        Field(label: "Provider") {
            FlowLayout(spacing: 8) {
                ForEach(Self.providers, id: \.self) { provider in
                    ProviderTile(provider: provider, selected: provider == draft.provider) { draft.provider = provider }
                }
            }
        }
        // An endpoint's key is the owner's to enter: a member sees no key field.
        if AccountDraft.supportsEndpoint(draft.provider) && targets.contains(where: \.owner) {
            Field(label: "Sign-in") {
                ChoiceChips(options: [
                    (value: false, label: "Log in", detail: "The provider’s own login"),
                    (value: true, label: "Use an endpoint", detail: "An API server and its key"),
                ], selection: $draft.endpoint)
                .accessibilityIdentifier("account-kind")
            }
        }
        Field(label: draft.usesEndpoint ? "Add on" : "Log in on") {
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
            .background(.card, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
        }
        Field(label: "Label") {
            InputBox(placeholder: ModelCatalog.providerName(draft.provider), text: $draft.label)
                .accessibilityIdentifier("account-label")
        }
        if draft.usesEndpoint && targets.contains(where: \.owner) {
            Field(label: "Endpoint URL") {
                InputBox(placeholder: "http://127.0.0.1:8317", text: $draft.url, mono: true)
                    .accessibilityIdentifier("account-endpoint")
            }
            Field(label: "Key") {
                InputBox(placeholder: "The server’s client key", text: $draft.key, mono: true, secure: true)
                    .accessibilityIdentifier("account-key")
            }
            Text("A CLIProxyAPI on this machine or the vault, or a local model server. The server holds the provider’s credentials; herder keeps only its key.")
                .font(.footnote).foregroundStyle(Theme.secondary)
                .fixedSize(horizontal: false, vertical: true)
        } else {
            DisclosureGroup(isExpanded: $showsConfigDir) {
                InputBox(placeholder: "Chosen by herder, such as ~/.\(draft.provider)-work", text: $draft.configDir, mono: true)
                    .padding(.top, 8)
            } label: {
                SectionHeading(title: "Config directory")
            }
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
                    } else if let failure = failures[target.hostId] {
                        Text(failure).font(.caption).foregroundStyle(Theme.failure)
                            .lineLimit(2).truncationMode(.tail)
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
