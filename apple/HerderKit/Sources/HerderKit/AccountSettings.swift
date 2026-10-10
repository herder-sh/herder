import Herder
import SwiftUI

struct AccountDraft {
    var id = ""
    var provider = "claude"
    var label = ""
    var configDir = ""

    mutating func fillId(taken: [String]) {
        id = Self.nextId(provider: provider, taken: taken)
    }

    static func nextId(provider: String, taken: [String]) -> String {
        if !taken.contains(provider) { return provider }
        var n = 2
        while taken.contains("\(provider)-\(n)") { n += 1 }
        return "\(provider)-\(n)"
    }

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
    var initialProvider: Provider = "claude"
    /// The account to start from, as one another machine has.
    var initialDraft: AccountDraft?
    @State private var draft = AccountDraft()
    @Environment(\.dismiss) private var dismiss

    private var machine: Machine? { fleet.machines.first { $0.hostId == hostId } }
    private var connection: TerminalConnection? { fleet.accountLogins[hostId] }
    private var taken: [String] { machine?.accounts.map(\.accountId) ?? [] }
    private var problem: String? { draft.problem(existing: taken) }
    private var canManage: Bool { machine?.role == .owner && machine?.connection == .connected }
    private var configDirHint: String {
        let defaultLogin = draft.provider == "claude" ? "; ~/.claude adds Claude’s default login" : ""
        return "A new directory, or one already logged in\(defaultLogin). Leave empty to let herder choose. The account appears once the provider reports it logged in."
    }

    var body: some View {
        SheetScaffold(title: sheetTitle, subtitle: machine?.name ?? "Machine", height: 680) {
            if let connection {
                Text(connection.install == nil
                     ? "Complete the provider’s login below. You can close this sheet and return to it from machine settings."
                     : "The vendor installer runs below. herder never installs silently. You can close this sheet and return to it from machine settings.")
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
                Field(label: "Account ID", hint: "A unique name on this machine, such as \(draft.provider)-work.") {
                    InputBox(placeholder: "\(draft.provider)-work", text: $draft.id, mono: true)
                }
                Field(label: "Display label") { InputBox(placeholder: "Work", text: $draft.label) }
                Field(label: "Config directory (optional)", hint: configDirHint) {
                    InputBox(placeholder: "~/.\(draft.provider)-work", text: $draft.configDir, mono: true)
                }
                if !draft.id.isEmpty, let problem {
                    Text(problem).font(.footnote).foregroundStyle(Theme.failure)
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
                    Text("Login runs on the selected machine.").font(.footnote).foregroundStyle(Theme.tertiary)
                }
                ActionButton(title: "Done", style: .primary) { dismiss() }
            } else {
                Spacer()
                ActionButton(title: "Start Login", style: .primary) {
                    guard canManage, problem == nil else { return }
                    fleet.accountLogins[hostId] = TerminalConnection(hostId: hostId, terminalId: nil, account: draft.account)
                }.frame(maxWidth: 180).disabled(!canManage || problem != nil)
            }
        }
        .onAppear {
            if draft.id.isEmpty {
                if let initialDraft {
                    draft = initialDraft
                } else {
                    draft.provider = initialProvider
                    draft.fillId(taken: taken)
                }
            }
        }
        .onChange(of: draft.provider) { old, new in
            if draft.id.isEmpty || draft.id == old || draft.id.hasPrefix(old + "-") {
                draft.fillId(taken: taken)
            }
            _ = new
        }
    }

    private var sheetTitle: String {
        if connection?.install != nil { return "Install Provider" }
        if connection?.relogin != nil { return "Log In Again" }
        return "Add Account"
    }
}

/// A machine to add an account on, and why this device cannot add one there, if it cannot.
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
    @State private var picked: HostId?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let targets = AccountTarget.all(fleet.machines)
        if let hostId = picked ?? AccountTarget.only(targets) {
            AddAccountSheet(fleet: fleet, hostId: hostId)
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

    /// This account's login, when one is running again from here.
    private var login: TerminalConnection? {
        fleet.accountLogins[hostId].flatMap { $0.relogin == account.accountId ? $0 : nil }
    }

    var body: some View {
        SheetScaffold(title: login == nil ? "Account Settings" : "Log In Again",
                      subtitle: "\(account.provider) · \(account.accountId)", height: login == nil ? 560 : 680) {
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
        .onAppear { label = account.label; configDir = account.configDir ?? ""; fallback = account.fallback }
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
        Field(label: "Rotation", hint: "A fallback login is picked for new sessions and failover only once every other \(account.provider) account is at its limit or logged out. Choosing it by name still uses it.") {
            ChoiceChips(options: [(false, "Use in rotation", ""), (true, "Fallback only", "")], selection: $fallback)
        }
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
                    configDir: path.isEmpty ? nil : path, fallback: fallback))
                dismiss()
            } catch { self.error = describe(error) }
        }.frame(maxWidth: 180).disabled(!canManage || pathProblem != nil || label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
    }
}
