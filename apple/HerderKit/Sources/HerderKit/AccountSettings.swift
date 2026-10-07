import Herder
import SwiftUI

struct AccountDraft {
    var id = ""
    var provider = "claude"
    var label = ""
    var configDir = ""

    var account: NewAccount {
        NewAccount(accountId: id.trimmingCharacters(in: .whitespacesAndNewlines), provider: provider,
                   label: clean(label), configDir: clean(configDir))
    }

    func problem(existing: [String]) -> String? {
        let value = account
        guard !value.accountId.isEmpty else { return "Enter an account ID." }
        guard value.accountId.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0)
            || (48...57).contains($0) || [45, 95, 46].contains($0) }) else {
            return "Account IDs use letters, numbers, hyphens, underscores and dots."
        }
        if existing.contains(value.accountId) { return "An account with this ID already exists." }
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

struct AddAccountSheet: View {
    let fleet: Fleet
    let hostId: HostId
    @State private var draft = AccountDraft()
    @State private var suggestedId = ""
    @State private var formError: String?
    @Environment(\.dismiss) private var dismiss

    private var machine: Machine? { fleet.machines.first { $0.hostId == hostId } }
    private var connection: TerminalConnection? { fleet.accountLogins[hostId] }
    private var problem: String? { draft.problem(existing: machine?.accounts.map(\.accountId) ?? []) }
    private var canManage: Bool { machine?.role == .owner && machine?.connection == .connected }
    private var status: ProviderStatus? {
        ModelCatalog.statusOn(fleet.machines, hostId: hostId, provider: draft.provider)
    }
    private var willInstall: Bool { status.map { !$0.installed && $0.canInstall } ?? false }

    var body: some View {
        SheetScaffold(title: sheetTitle, subtitle: machine?.name ?? "Machine", height: 680) {
            if let connection {
                Text(connection.install == nil
                     ? "Complete the provider’s login below. You can close this sheet and return to it from machine settings."
                     : "herder runs the vendor’s installer in this terminal. Then add the account again.")
                    .font(.footnote).foregroundStyle(Theme.secondary)
                TerminalSurface(connection: connection, client: fleet.client, sessionId: nil)
                    .frame(height: 390).background(.black)
                if let account = connection.account,
                   machine?.accounts.contains(where: { $0.accountId == account.accountId }) == true {
                    Label("Account added", systemImage: "checkmark.circle.fill").foregroundStyle(Theme.success)
                }
            } else {
                Field(label: "Provider") {
                    ChoiceChips(options: [("claude", "Claude", ""), ("codex", "Codex", ""),
                                          ("cursor", "Cursor", ""), ("opencode", "OpenCode", "")],
                                selection: $draft.provider)
                }
                Field(label: "Account ID", hint: "A unique name on this machine, such as claude-work.") {
                    InputBox(placeholder: suggestedId.isEmpty ? "claude-work" : suggestedId, text: $draft.id, mono: true)
                }
                Field(label: "Display label") { InputBox(placeholder: "Work", text: $draft.label) }
                Field(label: "Config directory (optional)",
                      hint: "A new directory, or one already logged in; ~/.claude adds Claude’s default login. Leave empty to let herder choose. The account appears once the provider reports it logged in.") {
                    InputBox(placeholder: "~/.claude-work", text: $draft.configDir, mono: true)
                }
                ForEach(providerNotes, id: \.self) { note in
                    Text(note).font(.footnote).foregroundStyle(Theme.secondary)
                }
                if !draft.id.isEmpty, let problem {
                    Text(problem).font(.footnote).foregroundStyle(Theme.failure)
                }
                if let formError {
                    Text(formError).font(.footnote).foregroundStyle(Theme.failure)
                }
                if !canManage {
                    Text("Connect as the machine owner to add an account.").foregroundStyle(Theme.secondary)
                }
            }
        } footer: {
            if let connection {
                switch connection.state {
                case .exited, .failed:
                    ActionButton(title: "Back", style: .secondary) { fleet.accountLogins[hostId] = nil }
                case .connecting, .attached:
                    Text(connection.install == nil
                         ? "Login runs on the selected machine."
                         : "Install runs on the selected machine.")
                        .font(.footnote).foregroundStyle(Theme.tertiary)
                }
                ActionButton(title: "Done", style: .primary) { dismiss() }
            } else {
                Spacer()
                ActionButton(title: willInstall ? "Install" : "Start Login", style: .primary) {
                    start()
                }.frame(maxWidth: 180).disabled(!canManage || (!willInstall && problem != nil))
            }
        }
        .onAppear { refreshSuggestion() }
        .onChange(of: draft.provider) { refreshSuggestion(clearingId: true) }
    }

    private var sheetTitle: String {
        if connection?.install != nil { return "Install Provider" }
        return connection?.relogin == nil ? "Add Account" : "Log In Again"
    }

    private var providerNotes: [String] {
        var notes: [String] = []
        let on = ModelCatalog.usedOn(fleet.machines, hostId: hostId, provider: draft.provider)
        if !on.isEmpty { notes.append("also on " + on.joined(separator: ", ")) }
        if let status {
            if status.installed {
                if let version = status.version { notes.append(version) }
                if let other = ModelCatalog.newerElsewhere(fleet.machines, hostId: hostId, provider: draft.provider) {
                    notes.append("\(draft.provider): newer on \(other.name)")
                }
            } else if status.canInstall {
                notes.append("not installed here; Install runs the vendor’s installer")
            } else {
                notes.append("not installed on this machine")
            }
        }
        return notes
    }

    private func refreshSuggestion(clearingId: Bool = false) {
        let next = ModelCatalog.nextAccountId(machine?.accounts ?? [], provider: draft.provider)
        if clearingId && (draft.id.isEmpty || draft.id == suggestedId) { draft.id = "" }
        suggestedId = next
    }

    private func start() {
        formError = nil
        guard canManage else { return }
        if willInstall {
            fleet.accountLogins[hostId] = TerminalConnection(
                hostId: hostId, terminalId: nil, install: draft.provider)
            return
        }
        if let status, !status.installed, !status.canInstall {
            formError = "install \(draft.provider) on this machine first"
            return
        }
        if draft.id.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            draft.id = suggestedId
        }
        guard problem == nil else { return }
        fleet.accountLogins[hostId] = TerminalConnection(
            hostId: hostId, terminalId: nil, account: draft.account)
    }
}

struct EditAccountSheet: View {
    let fleet: Fleet
    let hostId: HostId
    let account: Account
    @State private var label = ""
    @State private var configDir = ""
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    private var pathProblem: String? {
        let path = configDir.trimmingCharacters(in: .whitespacesAndNewlines)
        return path.isEmpty || AccountDraft.validPath(path) ? nil : "Use an absolute path or a path starting with ~/."
    }

    private var canManage: Bool {
        fleet.machines.contains { $0.hostId == hostId && $0.role == .owner && $0.connection == .connected }
    }

    /// This account's login, when one is running again from here.
    private var login: TerminalConnection? {
        fleet.accountLogins[hostId].flatMap { $0.relogin == account.accountId ? $0 : nil }
    }

    var body: some View {
        SheetScaffold(title: login == nil ? "Account Settings" : "Log In Again",
                      subtitle: "\(account.provider) · \(account.accountId)", height: login == nil ? 470 : 680) {
            if let login {
                Text("Complete the provider’s login below. It ends once the provider reports the account logged in; sessions can then use it again.")
                    .font(.footnote).foregroundStyle(Theme.secondary)
                TerminalSurface(connection: login, client: fleet.client, sessionId: nil)
                    .frame(height: 390).background(.black)
            } else {
                settings
            }
        } footer: {
            if let login {
                switch login.state {
                case .exited, .failed:
                    ActionButton(title: "Back", style: .secondary) { fleet.accountLogins[hostId] = nil }
                case .connecting, .attached:
                    Text("Login runs on the selected machine.").font(.footnote).foregroundStyle(Theme.tertiary)
                }
                ActionButton(title: "Done", style: .primary) { dismiss() }
            } else {
                ActionButton(title: "Log In Again", style: .secondary) {
                    guard canManage, !loginRunning else { return }
                    fleet.accountLogins[hostId] = TerminalConnection(hostId: hostId, terminalId: nil, relogin: account.accountId)
                }.frame(maxWidth: 180).disabled(!canManage || loginRunning)
                Spacer()
                save
            }
        }
        .onAppear { label = account.label; configDir = account.configDir ?? "" }
    }

    /// Whether another login runs on the machine, which this one would take the place of.
    private var loginRunning: Bool {
        guard let other = fleet.accountLogins[hostId] else { return false }
        switch other.state {
        case .exited, .failed: return false
        case .connecting, .attached: return true
        }
    }

    @ViewBuilder private var settings: some View {
        Field(label: "Display label") { InputBox(placeholder: account.accountId, text: $label) }
        Field(label: "Config directory", hint: "On this machine. Leave empty to use the provider’s default login. Archive all sessions on the machine before changing this directory.") {
            InputBox(placeholder: "Provider default", text: $configDir, mono: true)
        }
        if let pathProblem { Text(pathProblem).font(.footnote).foregroundStyle(Theme.failure) }
        if !canManage {
            Text("Connect as the machine owner to save changes.").font(.footnote).foregroundStyle(Theme.secondary)
        }
        if let error { Text(error).font(.footnote).foregroundStyle(Theme.failure) }
        if loginRunning {
            Text("Another login runs on this machine; finish it to log this account in again.")
                .font(.footnote).foregroundStyle(Theme.secondary)
        } else {
            Text("Log In Again runs the provider’s login in this account’s config directory, for a login that expired.")
                .font(.footnote).foregroundStyle(Theme.tertiary)
        }
    }

    private var save: some View {
        ActionButton(title: "Save", style: .primary) {
            guard canManage, pathProblem == nil else { return }
            do {
                let path = configDir.trimmingCharacters(in: .whitespacesAndNewlines)
                _ = try await fleet.client.send(hostId: hostId, command: .setAccountSettings(
                    accountId: account.accountId, label: label.trimmingCharacters(in: .whitespacesAndNewlines),
                    configDir: path.isEmpty ? nil : path))
                dismiss()
            } catch { self.error = describe(error) }
        }.frame(maxWidth: 180).disabled(!canManage || pathProblem != nil || label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
    }
}
