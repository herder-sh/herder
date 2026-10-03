import Herder
import SwiftUI
#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// The sheets the app presents.
enum AppSheet: Identifiable, Hashable {
    case pair
    /// A new session, in this project when given.
    case newSession(projectId: String?)
    /// A new project: a first session in a repository herder does not list yet.
    case newProject
    case projectSettings(projectId: String)
    case machineSettings(hostId: HostId)

    var id: Self { self }
}

extension AppSheet {
    @MainActor @ViewBuilder
    func view(fleet: Fleet, opened: @escaping (SessionKey) -> Void) -> some View {
        switch self {
        case .pair: PairSheet(fleet: fleet)
        case .newSession(let projectId): NewSessionSheet(fleet: fleet, projectId: projectId, newProject: false, opened: opened)
        case .newProject: NewSessionSheet(fleet: fleet, projectId: nil, newProject: true, opened: opened)
        case .projectSettings(let projectId): ProjectSettingsSheet(fleet: fleet, projectId: projectId)
        case .machineSettings(let hostId): MachineSettingsSheet(fleet: fleet, hostId: hostId)
        }
    }
}

/// Pairs with a machine from the `herder://pair` link `herder pair` prints.
struct PairSheet: View {
    let fleet: Fleet
    @Environment(\.dismiss) private var dismiss
    @State private var link = ""
    @State private var error: String?

    var body: some View {
        SheetScaffold(title: "Add Machine", subtitle: "Pair this device with a machine running herder.",
                      height: uri == nil ? 440 : 600) {
            Field(label: "1 · On the machine") {
                HStack {
                    Text("herder pair").font(Theme.mono).foregroundStyle(Theme.text)
                    Spacer()
                    CopyButton(text: "herder pair")
                }
                .padding(12)
                .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
            }
            Field(label: "2 · Paste the link it prints") {
                VStack(alignment: .trailing, spacing: 8) {
                    InputBox(placeholder: "herder://pair?host=…&fp=…&code=…", text: $link, mono: true, lines: 3...5)
                        .accessibilityIdentifier("pairing-link")
                    Button("Paste", systemImage: "doc.on.clipboard") { link = Clipboard.string ?? link }
                        .buttonStyle(.plain)
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(Theme.secondary)
                }
            }
            if let uri {
                Field(label: "3 · Check it is your machine",
                      hint: "The fingerprint must match the one `herder pair` printed.") {
                    VStack(alignment: .leading, spacing: 10) {
                        DetailRow(label: "Addresses", value: uri.hosts.joined(separator: "\n"), mono: true)
                        DetailRow(label: "Fingerprint", value: grouped(uri.fingerprint), mono: true)
                        if let paired = fleet.machines.first(where: { $0.fingerprint == uri.fingerprint }) {
                            Text("Already paired as \(paired.name): pairing again gives it a new key.")
                                .font(.footnote)
                                .foregroundStyle(Theme.accent)
                        }
                    }
                    .padding(12)
                    .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
                }
            } else if !trimmed.isEmpty {
                Text("That is not a herder pairing link.").font(.footnote).foregroundStyle(Theme.failure)
            }
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
            }
        } footer: {
            Spacer()
            ActionButton(title: "Pair", style: .primary) { await pair() }
                .frame(maxWidth: 200)
                .disabled(uri == nil)
                .opacity(uri == nil ? 0.4 : 1)
                .keyboardShortcut(.defaultAction)
        }
    }

    private var trimmed: String { link.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var uri: PairingUri? { try? parsePairingUri(link: trimmed) }

    private func pair() async {
        do {
            try await fleet.pair(link: trimmed)
            dismiss()
        } catch {
            self.error = describe(error)
        }
    }
}

/// A fingerprint in groups of four, easier to compare by eye.
private func grouped(_ fingerprint: String) -> String {
    stride(from: 0, to: fingerprint.count, by: 4).map { start in
        let from = fingerprint.index(fingerprint.startIndex, offsetBy: start)
        return String(fingerprint[from..<(fingerprint.index(from, offsetBy: 4, limitedBy: fingerprint.endIndex) ?? fingerprint.endIndex)])
    }
    .joined(separator: " ")
}

/// Starts a session: on a machine, in one of its projects or any repository there, on one of
/// its accounts, optionally with a first prompt.
struct NewSessionSheet: View {
    let fleet: Fleet
    let projectId: String?
    let newProject: Bool
    let opened: (SessionKey) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var hostId: HostId = ""
    @State private var project: String = ""
    @State private var repo = ""
    @State private var accountId: AccountId = ""
    @State private var model = ""
    @State private var mode: PermissionMode = .ask
    @State private var prompt = ""
    @State private var error: String?

    /// Machines that run sessions: connected, and not a vault.
    private var machines: [Machine] {
        fleet.machines.filter { $0.connection == .connected && $0.hosts.isEmpty }
    }

    private var machine: Machine? { machines.first { $0.hostId == hostId } }
    private static let otherRepository = "\u{0}other"

    var body: some View {
        SheetScaffold(
            title: newProject ? "New Project" : "New Session",
            subtitle: newProject
                ? "herder finds projects from the repositories its sessions run in. Start one in a repository on the machine."
                : "Start an agent on a fresh worktree and branch."
        ) {
            if machines.isEmpty {
                Text("No connected machine can run sessions.").foregroundStyle(Theme.secondary)
            } else {
                Field(label: "Machine") {
                    ChoiceChips(options: machines.map { ($0.hostId, $0.name, $0.accounts.count == 1 ? "1 account" : "\($0.accounts.count) accounts") },
                                selection: $hostId)
                }
                if let machine {
                    if !newProject {
                        Field(label: "Project") {
                            ChoiceChips(
                                options: machine.projects.map { ($0.projectId, $0.name, $0.paths.first ?? "") }
                                    + [(Self.otherRepository, "Other repository…", "")],
                                selection: $project)
                        }
                    }
                    if newProject || project == Self.otherRepository {
                        Field(label: "Repository", hint: "An absolute path to a git repository on \(machine.name).") {
                            InputBox(placeholder: "/home/you/src/project", text: $repo, mono: true)
                        }
                    }
                    Field(label: "Account") {
                        if machine.accounts.isEmpty {
                            Text("This machine has no accounts yet.").foregroundStyle(Theme.secondary)
                        } else {
                            ChoiceChips(options: machine.accounts.map { ($0.accountId, $0.label, $0.provider) },
                                        selection: $accountId)
                        }
                    }
                    Field(label: "Model", hint: "Blank uses the provider's default.") {
                        InputBox(placeholder: "Provider's default", text: $model, mono: true)
                    }
                    Field(label: "Permissions") {
                        ChoiceChips(options: [
                            (PermissionMode.readOnly, "Read only", "No writes"),
                            (.ask, "Ask", "Approve each change"),
                            (.autoEdit, "Auto edit", "Edits without asking"),
                            (.fullAccess, "Full access", "Everything"),
                        ], selection: $mode)
                    }
                    Field(label: "First prompt", hint: "Optional; the session starts idle without one.") {
                        InputBox(placeholder: "What should the agent do?", text: $prompt, lines: 3...8)
                    }
                }
            }
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
            }
        } footer: {
            Spacer()
            ActionButton(title: newProject ? "Start Project" : "Start Session", style: .primary) { await start() }
                .frame(maxWidth: 220)
                .disabled(!ready)
                .opacity(ready ? 1 : 0.4)
                .keyboardShortcut(.defaultAction)
        }
        .onAppear(perform: preselect)
        .onChange(of: hostId) { preselectFor(machine) }
    }

    private var usesRepository: Bool { newProject || project == Self.otherRepository }

    private var ready: Bool {
        guard let machine, !machine.accounts.isEmpty, !accountId.isEmpty else { return false }
        return usesRepository ? repo.trimmingCharacters(in: .whitespaces).hasPrefix("/") : !project.isEmpty
    }

    private func preselect() {
        let withProject = projectId.flatMap { id in machines.first { $0.projects.contains { $0.projectId == id } } }
        hostId = (withProject ?? machines.first)?.hostId ?? ""
        preselectFor(machine)
    }

    private func preselectFor(_ machine: Machine?) {
        guard let machine else { return }
        let projects = machine.projects
        project = projects.first { $0.projectId == projectId }?.projectId ?? projects.first?.projectId ?? Self.otherRepository
        let preferred = projects.first { $0.projectId == project }?.defaultAccount
        accountId = machine.accounts.first { $0.accountId == preferred }?.accountId ?? machine.accounts.first?.accountId ?? ""
    }

    private func start() async {
        do {
            let key = try await fleet.createSession(
                on: hostId,
                repo: usesRepository ? repo.trimmingCharacters(in: .whitespaces) : nil,
                projectId: usesRepository ? nil : project,
                accountId: accountId,
                model: model.trimmingCharacters(in: .whitespaces),
                mode: mode,
                prompt: prompt.trimmingCharacters(in: .whitespacesAndNewlines))
            opened(key)
            dismiss()
        } catch {
            self.error = describe(error)
        }
    }
}

/// A project's settings on each machine that has it. They are set in each machine's
/// `daemon.toml`; the protocol has no command to change them yet.
struct ProjectSettingsSheet: View {
    let fleet: Fleet
    let projectId: String

    var body: some View {
        let group = fleet.lists.projects.first { $0.id == projectId }
        SheetScaffold(title: group?.name ?? "Project", subtitle: projectId) {
            ForEach(fleet.machines.filter { $0.projects.contains { $0.projectId == projectId } }, id: \.hostId) { machine in
                if let project = machine.projects.first(where: { $0.projectId == projectId }) {
                    Field(label: machine.name) {
                        VStack(alignment: .leading, spacing: 10) {
                            DetailRow(label: "Name", value: project.name)
                            DetailRow(label: "Clones", value: project.paths.joined(separator: "\n"), mono: true)
                            DetailRow(label: "Default account",
                                      value: project.defaultAccount.flatMap { id in
                                          machine.accounts.first { $0.accountId == id }?.label ?? id } ?? "")
                            DetailRow(label: "Setup command", value: project.setupCommand ?? "", mono: true)
                        }
                        .padding(12)
                        .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
                    }
                }
            }
            Text("These are set in each machine's `daemon.toml`, under `[[project]]`.")
                .font(.footnote)
                .foregroundStyle(Theme.tertiary)
        } footer: {
            Spacer()
        }
    }
}

/// A machine: its name on this device, its connection, accounts and identity; forget it.
struct MachineSettingsSheet: View {
    let fleet: Fleet
    let hostId: HostId
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var confirmingForget = false
    @State private var error: String?

    var body: some View {
        let machine = fleet.machines.first { $0.hostId == hostId }
        SheetScaffold(title: machine?.name ?? "Machine", subtitle: machine?.connection.label ?? "") {
            if let machine {
                Field(label: "Name on this device") {
                    HStack(spacing: 10) {
                        InputBox(placeholder: machine.name, text: $name)
                        ActionButton(title: "Rename", style: .secondary) {
                            perform { try fleet.rename(hostId, to: name.trimmingCharacters(in: .whitespaces)) }
                        }
                        .frame(width: 120)
                        .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty || name == machine.name)
                    }
                }
                Field(label: "Machine") {
                    VStack(alignment: .leading, spacing: 10) {
                        DetailRow(label: "Your role", value: machine.role.map { $0 == .owner ? "Owner" : "Member" }
                                  ?? "Known once connected")
                        DetailRow(label: "Addresses", value: machine.addresses.joined(separator: "\n"), mono: true)
                        DetailRow(label: "Fingerprint", value: grouped(machine.fingerprint), mono: true)
                        DetailRow(label: "Host id", value: machine.hostId, mono: true)
                        if !machine.hosts.isEmpty {
                            DetailRow(label: "Vault of", value: machine.hosts.map(\.hostName).joined(separator: ", "))
                        }
                    }
                    .padding(12)
                    .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
                }
                Field(label: "Accounts",
                      hint: "Accounts and failover are set in the machine's `daemon.toml`.") {
                    VStack(alignment: .leading, spacing: 8) {
                        if machine.accounts.isEmpty {
                            Text("No accounts yet").foregroundStyle(Theme.tertiary)
                        }
                        ForEach(machine.accounts, id: \.accountId) { account in
                            HStack {
                                Text(account.label).foregroundStyle(Theme.text)
                                Text(account.provider).foregroundStyle(Theme.secondary)
                                if account.failover { Chip(text: "failover") }
                                Spacer()
                                Text(account.accountId).font(Theme.monoSmall).foregroundStyle(Theme.tertiary)
                            }
                            .font(.subheadline)
                        }
                        if machine.failover.pin {
                            Label("Failover pinned: sessions stay on their account", systemImage: "pin")
                                .font(.footnote).foregroundStyle(Theme.tertiary)
                        }
                    }
                    .padding(12)
                    .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
                }
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Theme.failure)
                }
            }
        } footer: {
            ActionButton(title: "Forget Machine", style: .secondary) { confirmingForget = true }
                .frame(maxWidth: 200)
                .foregroundStyle(Theme.failure)
            Spacer()
        }
        .onAppear { name = machine?.name ?? "" }
        .confirmationDialog("Forget \(machine?.name ?? "") on this device?", isPresented: $confirmingForget,
                            titleVisibility: .visible) {
            Button("Forget", role: .destructive) {
                perform { try fleet.forget(hostId) }
                dismiss()
            }
        } message: {
            Text("Pair again to get it back.")
        }
    }

    private func perform(_ action: () throws -> Void) {
        do {
            try action()
            error = nil
        } catch {
            self.error = describe(error)
        }
    }
}

/// Copies text to the clipboard, confirming with a check mark.
struct CopyButton: View {
    let text: String
    @State private var copied = false

    var body: some View {
        Button {
            Clipboard.string = text
            copied = true
        } label: {
            Image(systemName: copied ? "checkmark" : "doc.on.doc")
                .foregroundStyle(Theme.secondary)
                .frame(width: 30, height: 30)
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .help("Copy")
    }
}

enum Clipboard {
    @MainActor static var string: String? {
        get {
            #if os(iOS)
            UIPasteboard.general.string
            #else
            NSPasteboard.general.string(forType: .string)
            #endif
        }
        set {
            #if os(iOS)
            UIPasteboard.general.string = newValue
            #else
            NSPasteboard.general.clearContents()
            if let newValue { NSPasteboard.general.setString(newValue, forType: .string) }
            #endif
        }
    }
}
