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

/// Picks where a new session runs, as a palette: one row per project, across machines, then a
/// repository path on a machine for a new project. The chat opens on the machine the project
/// was used on last; it can change there.
struct ProjectPicker: View {
    let fleet: Fleet
    let newProject: Bool
    let picked: (Draft) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var other = false
    @State private var hostId: HostId = ""
    @State private var repo = ""
    @FocusState private var searching: Bool

    /// Machines that run sessions: connected, and not a vault.
    private var machines: [Machine] {
        fleet.machines.filter { $0.connection == .connected && $0.hosts.isEmpty }
    }

    /// Each project once, with the machines that have it.
    private var projects: [(id: String, name: String, machines: [Machine])] {
        var order: [String] = []
        var groups: [String: (name: String, machines: [Machine])] = [:]
        for machine in machines {
            for project in machine.projects {
                if groups[project.projectId] == nil { order.append(project.projectId) }
                groups[project.projectId, default: (project.name, [])].machines.append(machine)
            }
        }
        let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
        return order.compactMap { id in groups[id].map { (id, $0.name, $0.machines) } }
            .filter { needle.isEmpty || $0.name.lowercased().contains(needle) }
            .sorted { $0.name.lowercased() < $1.name.lowercased() }
    }

    /// The machine a project's newest session ran on, else the first that has it.
    private func machine(for projectId: String, among candidates: [Machine]) -> Machine? {
        let newest = fleet.lists.projects.first { $0.projectId == projectId }?.sessions
            .compactMap { session in fleet.sessions[session.key].map { (session.key.hostId, $0.updatedAt ?? .distantPast) } }
            .max { $0.1 < $1.1 }?.0
        return candidates.first { $0.hostId == newest } ?? candidates.first
    }

    var body: some View {
        VStack(spacing: 0) {
            if newProject || other {
                VStack(alignment: .leading, spacing: 16) {
                    Text("New project").font(.headline).foregroundStyle(Theme.text)
                    Field(label: "Machine") {
                        ChoiceChips(options: machines.map { ($0.hostId, $0.name, "") }, selection: $hostId)
                    }
                    Field(label: "Repository", hint: "An absolute path to a git repository on the machine.") {
                        InputBox(placeholder: "/home/you/src/project", text: $repo, mono: true)
                    }
                    HStack {
                        Spacer()
                        ActionButton(title: "Continue", style: .primary) { submitPath() }
                            .frame(maxWidth: 160)
                            .disabled(!pathReady)
                            .opacity(pathReady ? 1 : 0.4)
                            .keyboardShortcut(.defaultAction)
                    }
                }
                .padding(20)
            } else {
                HStack(spacing: 10) {
                    Image(systemName: "magnifyingglass").foregroundStyle(Theme.tertiary)
                    TextField("Start a session in…", text: $query)
                        .textFieldStyle(.plain)
                        .font(.title3)
                        .foregroundStyle(Theme.text)
                        .focused($searching)
                        .onSubmit {
                            if let first = projects.first { pick(first.id, first.machines) }
                        }
                }
                .padding(.horizontal, 18)
                .frame(height: 56)
                Rectangle().fill(Theme.stroke).frame(height: 1)
                ScrollView {
                    VStack(alignment: .leading, spacing: 2) {
                        ForEach(projects, id: \.id) { project in
                            PickRow(title: project.name, detail: project.machines.map(\.name).joined(separator: ", "),
                                    symbol: "shippingbox") { pick(project.id, project.machines) }
                        }
                        PickRow(title: "Other repository…", detail: "", symbol: "folder.badge.plus") { other = true }
                    }
                    .padding(8)
                }
                .frame(maxHeight: 360)
            }
        }
        .frame(width: 520)
        .fixedSize(horizontal: false, vertical: true)
        .background(Theme.surface)
        .preferredColorScheme(.dark)
        .onAppear {
            hostId = machines.first?.hostId ?? ""
            searching = true
        }
    }

    private func pick(_ projectId: String, _ candidates: [Machine]) {
        guard let machine = machine(for: projectId, among: candidates) else { return }
        picked(Draft(hostId: machine.hostId, projectId: projectId))
        dismiss()
    }

    private var pathReady: Bool { !hostId.isEmpty && repo.trimmingCharacters(in: .whitespaces).hasPrefix("/") }

    private func submitPath() {
        picked(Draft(hostId: hostId, repo: repo.trimmingCharacters(in: .whitespaces)))
        dismiss()
    }
}

private struct PickRow: View {
    let title: String
    let detail: String
    let symbol: String
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: symbol).foregroundStyle(Theme.secondary).frame(width: 18)
                Text(title).font(.body.weight(.medium)).foregroundStyle(Theme.text)
                Spacer()
                Text(detail).font(.footnote).foregroundStyle(Theme.tertiary).lineLimit(1)
            }
            .padding(.horizontal, 12)
            .frame(height: 40)
            .background(hovering ? Theme.raised : .clear, in: .rect(cornerRadius: 8))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
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
