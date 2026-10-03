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
    /// A new session: pick its project first.
    case newSession
    /// A new project: a first session in a repository herder does not list yet.
    case newProject
    case projectSettings(projectId: String)
    case machineSettings(hostId: HostId)

    var id: Self { self }
}

extension AppSheet {
    @MainActor @ViewBuilder
    func view(fleet: Fleet, drafted: @escaping (Draft) -> Void) -> some View {
        switch self {
        case .pair: PairSheet(fleet: fleet)
        case .newSession: ProjectPicker(fleet: fleet, newProject: false, picked: drafted)
        case .newProject: ProjectPicker(fleet: fleet, newProject: true, picked: drafted)
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

/// A session about to start: where it runs. The chat opens empty, with its account, model
/// and permissions preselected; the first prompt creates it.
struct Draft: Hashable, Identifiable {
    var hostId: HostId
    /// The project to start in, or `nil` for `repo`.
    var projectId: String?
    var repo: String?
    var id: String { "\(hostId)/\(projectId ?? repo ?? "")" }

    /// A draft in a project, on the first connected machine that has it.
    @MainActor
    static func inProject(_ projectId: String, fleet: Fleet) -> Draft? {
        fleet.machines.first { $0.connection == .connected && $0.projects.contains { $0.projectId == projectId } }
            .map { Draft(hostId: $0.hostId, projectId: projectId) }
    }
}

/// Picks where a new session runs: a project (on the machine that has it), or a repository
/// path on a machine for a new project.
struct ProjectPicker: View {
    let fleet: Fleet
    let newProject: Bool
    let picked: (Draft) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var other = false
    @State private var hostId: HostId = ""
    @State private var repo = ""

    /// Machines that run sessions: connected, and not a vault.
    private var machines: [Machine] {
        fleet.machines.filter { $0.connection == .connected && $0.hosts.isEmpty }
    }

    var body: some View {
        SheetScaffold(
            title: newProject ? "New Project" : "New Session",
            subtitle: newProject ? "Start in a repository on one of your machines." : "Pick where it runs.",
            height: 480
        ) {
            if !newProject && !other {
                VStack(spacing: 6) {
                    ForEach(machines, id: \.hostId) { machine in
                        ForEach(machine.projects, id: \.projectId) { project in
                            PickRow(title: project.name, detail: machine.name, symbol: "shippingbox") {
                                picked(Draft(hostId: machine.hostId, projectId: project.projectId))
                                dismiss()
                            }
                        }
                    }
                    PickRow(title: "Other repository…", detail: "Any git repository on a machine", symbol: "folder") {
                        other = true
                    }
                }
            } else {
                Field(label: "Machine") {
                    ChoiceChips(options: machines.map { ($0.hostId, $0.name, "") }, selection: $hostId)
                }
                Field(label: "Repository", hint: "An absolute path to a git repository on the machine.") {
                    InputBox(placeholder: "/home/you/src/project", text: $repo, mono: true)
                }
            }
        } footer: {
            if newProject || other {
                Spacer()
                ActionButton(title: "Continue", style: .primary) {
                    picked(Draft(hostId: hostId, repo: repo.trimmingCharacters(in: .whitespaces)))
                    dismiss()
                }
                .frame(maxWidth: 200)
                .disabled(!ready)
                .opacity(ready ? 1 : 0.4)
                .keyboardShortcut(.defaultAction)
            }
        }
        .onAppear { hostId = machines.first?.hostId ?? "" }
    }

    private var ready: Bool { !hostId.isEmpty && repo.trimmingCharacters(in: .whitespaces).hasPrefix("/") }
}

private struct PickRow: View {
    let title: String
    let detail: String
    let symbol: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: symbol).foregroundStyle(Theme.secondary).frame(width: 20)
                Text(title).font(.body.weight(.semibold)).foregroundStyle(Theme.text)
                Spacer()
                Text(detail).font(.footnote).foregroundStyle(Theme.tertiary)
                Image(systemName: "chevron.right").font(.footnote).foregroundStyle(Theme.tertiary)
            }
            .padding(.horizontal, 14)
            .frame(minHeight: 48)
            .background(Theme.raised, in: .rect(cornerRadius: Theme.corner))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
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
