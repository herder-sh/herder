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
    /// A link that pairs another device with this one's machines.
    case share
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
        case .share: ShareSheet(fleet: fleet)
        case .newSession: ProjectPicker(fleet: fleet, newProject: false, picked: drafted)
        case .newProject: ProjectPicker(fleet: fleet, newProject: true, picked: drafted)
        case .projectSettings(let projectId): ProjectSettingsSheet(fleet: fleet, projectId: projectId)
        case .machineSettings(let hostId): MachineSettingsSheet(fleet: fleet, hostId: hostId)
        }
    }
}

/// Pairs with the machines a `herder://pair` link names: the one `herder pair` prints, or one
/// another device shares for all of its machines.
struct PairSheet: View {
    let fleet: Fleet
    @Environment(\.dismiss) private var dismiss
    @State private var link = ""
    @State private var error: String?
    @State private var scanning = false
    /// Each machine's result, once a link of several machines, or one that failed, paired.
    @State private var results: [PairResult]?

    var body: some View {
        SheetScaffold(title: "Add Machine", subtitle: "Pair this device with a machine running herder.",
                      height: machines == nil ? 440 : 600) {
            if let results {
                Field(label: "Results") {
                    VStack(alignment: .leading, spacing: 10) {
                        ForEach(Array(results.enumerated()), id: \.offset) { PairResultRow(result: $0.element) }
                    }
                    .padding(12)
                    .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
                }
            } else {
                steps
            }
        } footer: {
            Spacer()
            if results == nil {
                ActionButton(title: "Pair", style: .primary) { await pair() }
                    .frame(maxWidth: 200)
                    .disabled(machines == nil)
                    .opacity(machines == nil ? 0.4 : 1)
                    .keyboardShortcut(.defaultAction)
            } else {
                ActionButton(title: "Done", style: .primary) { dismiss() }
                    .frame(maxWidth: 200)
                    .keyboardShortcut(.defaultAction)
            }
        }
        #if os(iOS)
        .fullScreenCover(isPresented: $scanning) { PairScanner { link = $0 } }
        #endif
    }

    @ViewBuilder private var steps: some View {
        Field(label: "1 · On the machine") {
            HStack {
                Text("herder pair").font(Theme.mono).foregroundStyle(Theme.text)
                Spacer()
                CopyButton(text: "herder pair")
            }
            .padding(12)
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        }
        Field(label: scanLabel,
              hint: "Or, on a device already paired, open Machines › Pair Another Device.") {
            VStack(alignment: .trailing, spacing: 8) {
                #if os(iOS)
                ActionButton(title: "Scan QR Code", style: .primary) { scanning = true }
                    .accessibilityIdentifier("scan-pairing-code")
                #endif
                InputBox(placeholder: "herder://pair?host=…&fp=…&code=…", text: $link, mono: true, lines: 3...5)
                    .accessibilityIdentifier("pairing-link")
                Button("Paste", systemImage: "doc.on.clipboard") { link = Clipboard.string ?? link }
                    .buttonStyle(.plain)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.secondary)
            }
        }
        if let machines {
            Field(label: machines.count == 1 ? "3 · Check it is your machine" : "3 · Check they are your \(machines.count) machines",
                  hint: "A fingerprint must match the one `herder pair` printed on its machine.") {
                VStack(alignment: .leading, spacing: 14) {
                    ForEach(Array(machines.enumerated()), id: \.offset) { _, uri in
                        VStack(alignment: .leading, spacing: 10) {
                            DetailRow(label: "Addresses", value: uri.hosts.joined(separator: "\n"), mono: true)
                            DetailRow(label: "Fingerprint", value: grouped(uri.fingerprint), mono: true)
                            if let paired = fleet.machines.first(where: { $0.fingerprint == uri.fingerprint }) {
                                Text("Already paired as \(paired.name): pairing again gives it a new key.")
                                    .font(.footnote)
                                    .foregroundStyle(Theme.accent)
                            }
                        }
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
    }

    #if os(iOS)
    private let scanLabel = "2 · Scan the QR code it prints, or paste the link"
    #else
    private let scanLabel = "2 · Paste the link it prints"
    #endif
    private var trimmed: String { link.trimmingCharacters(in: .whitespacesAndNewlines) }
    /// The link in what was pasted, which may be all of `herder pair`'s output.
    private var found: String? { pairingLink(in: link) }
    private var machines: [PairingUri]? { found.flatMap { try? parsePairingLink(link: $0) }?.machines }

    private func pair() async {
        guard let found else { return }
        do {
            let results = try await fleet.pair(link: found)
            // One machine that paired needs no report: it shows in the list.
            if results.count == 1, case .paired = results[0] {
                dismiss()
            } else {
                self.results = results
            }
        } catch {
            self.error = describe(error)
        }
    }
}

/// One machine of a link: paired, or why not.
private struct PairResultRow: View {
    let result: PairResult

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            switch result {
            case .paired(let machine):
                Image(systemName: "checkmark.circle.fill").foregroundStyle(Theme.success)
                VStack(alignment: .leading, spacing: 2) {
                    Text(machine.name).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    Text("Paired").font(.caption).foregroundStyle(Theme.secondary)
                }
            case .failed(let addresses, let error):
                Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.failure)
                VStack(alignment: .leading, spacing: 2) {
                    Text(addresses.first ?? "A machine").font(Theme.mono).foregroundStyle(Theme.text)
                    Text("Not paired: \(error)").font(.caption).foregroundStyle(Theme.failure)
                }
            }
            Spacer(minLength: 0)
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

    /// What creating its session names: the project when known, else the path; the daemon
    /// takes exactly one. A just-added project keeps its path only to show it.
    var createArguments: (repo: String?, projectId: String?) {
        projectId == nil ? (repo, nil) : (nil, projectId)
    }

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
    @State private var addError: String?
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
                // Scrolls on iOS, so the keyboard does not push its heading off the sheet.
                #if os(iOS)
                ScrollView { pathForm }
                #else
                pathForm
                #endif
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
                                    symbol: "shippingbox", icon: ProjectIcon(projectId: project.id, name: project.name, image: fleet.projectIcon(project.id))) {
                                pick(project.id, project.machines)
                            }
                        }
                        PickRow(title: "Other repository…", detail: "", symbol: "folder.badge.plus") { other = true }
                    }
                    .padding(8)
                }
                #if os(macOS)
                .frame(maxHeight: 360)
                #endif
            }
        }
        // A palette on the Mac; on iOS it fills the system sheet.
        #if os(macOS)
        .frame(width: 520)
        .fixedSize(horizontal: false, vertical: true)
        #else
        .frame(maxHeight: .infinity, alignment: .top)
        #endif
        .background(Theme.surface)
        .preferredColorScheme(.dark)
        .onAppear {
            hostId = machines.first?.hostId ?? ""
            searching = true
        }
    }

    /// A repository path on a machine, for a new project.
    private var pathForm: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("New project").font(.headline).foregroundStyle(Theme.text)
            Field(label: "Machine") {
                ChoiceChips(options: machines.map { ($0.hostId, $0.name, "") }, selection: $hostId)
            }
            Field(label: "Repository", hint: "Pick a git repository on the machine, or type its path.") {
                VStack(spacing: 8) {
                    InputBox(placeholder: "/home/you/src/project", text: $repo, mono: true)
                    FolderBrowser(fleet: fleet, hostId: hostId, picked: $repo)
                        .id(hostId)
                        .frame(height: 220)
                        // Another machine's path means nothing here: start at its home.
                        .onChange(of: hostId) { repo = "" }
                }
            }
            if let addError {
                Text(addError).font(.footnote).foregroundStyle(Theme.failure)
            }
            HStack {
                Spacer()
                ActionButton(title: "Add Project", style: .primary) { await submitPath() }
                    .frame(maxWidth: 160)
                    .disabled(!pathReady)
                    .opacity(pathReady ? 1 : 0.4)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
    }

    private func pick(_ projectId: String, _ candidates: [Machine]) {
        guard let machine = machine(for: projectId, among: candidates) else { return }
        picked(Draft(hostId: machine.hostId, projectId: projectId))
        dismiss()
    }

    private var pathReady: Bool {
        let path = repo.trimmingCharacters(in: .whitespaces)
        return !hostId.isEmpty && (path.hasPrefix("/") || path.hasPrefix("~/"))
    }

    /// Registers the repository as a project on the machine, then opens a chat in it.
    private func submitPath() async {
        let path = repo.trimmingCharacters(in: .whitespaces)
        do {
            let projectId = try await fleet.addProject(path, on: hostId)
            picked(Draft(hostId: hostId, projectId: projectId, repo: path))
            dismiss()
        } catch {
            addError = describe(error)
        }
    }
}

private struct PickRow: View {
    let title: String
    let detail: String
    let symbol: String
    /// A project's tile, in the symbol's place.
    var icon: ProjectIcon?
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Group {
                    if let icon { icon } else { Image(systemName: symbol).foregroundStyle(Theme.secondary) }
                }
                .frame(width: 20)
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

/// A project's settings on each machine that has it, editable by the machine's owners: the
/// permissions and account new sessions start with, and the command a new worktree runs first.
/// Changes apply as they are made.
struct ProjectSettingsSheet: View {
    let fleet: Fleet
    let projectId: String
    @State private var hostId: HostId?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let group = fleet.lists.projects.first { $0.id == projectId }
        let machines = fleet.machines.filter { $0.projects.contains { $0.projectId == projectId } }
        let machine = machines.first { $0.hostId == hostId } ?? machines.first
        SheetScaffold(title: group?.name ?? "Project", subtitle: projectId, height: 520) {
            if machines.count > 1 {
                MachineTabs(machines: machines, selection: Binding(get: { machine?.hostId }, set: { hostId = $0 }))
            }
            if let machine, let project = machine.projects.first(where: { $0.projectId == projectId }) {
                ProjectSettingsForm(fleet: fleet, machine: machine, project: project)
                    .id(machine.hostId)
            } else {
                Text("No connected machine has this project.").foregroundStyle(Theme.secondary)
            }
        } footer: {
            if let machine {
                Label(machine.role == .owner ? "Saved on \(machine.name) as you change it"
                                             : "Only \(machine.name)'s owners can change these",
                      systemImage: machine.role == .owner ? "checkmark.circle" : "lock")
                    .font(.footnote).foregroundStyle(Theme.tertiary)
            }
            Spacer()
        }
        // Removed from its last machine: nothing is left to set.
        .onChange(of: machines.isEmpty) { if machines.isEmpty { dismiss() } }
    }
}

/// One tab per machine, when a project is on several.
struct MachineTabs: View {
    let machines: [Machine]
    @Binding var selection: HostId?

    var body: some View {
        HStack(spacing: 4) {
            ForEach(machines, id: \.hostId) { machine in
                let selected = machine.hostId == selection
                Button { selection = machine.hostId } label: {
                    HStack(spacing: 6) {
                        ConnectionMark(state: machine.connection)
                        Text(machine.name).lineLimit(1)
                    }
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(selected ? Theme.text : Theme.secondary)
                    .padding(.horizontal, 10)
                    .frame(height: 30)
                    .background(selected ? Theme.raised : .clear, in: .rect(cornerRadius: 7))
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(3)
        .background(Theme.background, in: .rect(cornerRadius: 9))
    }
}

struct ProjectSettingsForm: View {
    let fleet: Fleet
    let machine: Machine
    let project: Project
    @State private var mode: PermissionMode?
    @State private var account: AccountId?
    @State private var setup = ""
    @State private var loaded = false
    @State private var error: String?
    @State private var confirmingRemove = false
    @FocusState private var editingSetup: Bool

    var body: some View {
        let owner = machine.role == .owner
        VStack(alignment: .leading, spacing: 18) {
            SettingsGroup(title: "New sessions") {
                SettingRow(label: "Permissions", detail: "What agents may do without asking") {
                    FooterItem(symbol: "lock.shield", text: mode?.label ?? "Ask each time") {
                        Picker("Permissions", selection: $mode) {
                            Text("Ask each time").tag(PermissionMode?.none)
                            ForEach([PermissionMode.readOnly, .ask, .autoEdit, .fullAccess], id: \.self) {
                                Text($0.label).tag(Optional($0))
                            }
                        }
                        .pickerStyle(.inline)
                    }
                }
                RowDivider()
                SettingRow(label: "Account", detail: "Rotates to another when it runs out") {
                    FooterItem(symbol: "person.crop.circle", text: accountLabel) {
                        Picker("Account", selection: $account) {
                            Text("Most room left").tag(AccountId?.none)
                            ForEach(machine.accounts, id: \.accountId) { account in
                                Text("\(account.label) · \(account.provider)").tag(Optional(account.accountId))
                            }
                        }
                        .pickerStyle(.inline)
                    }
                }
                RowDivider()
                SettingRow(label: "Setup command", detail: "Runs once in each new worktree") {
                    TextField("make bootstrap", text: $setup)
                        .textFieldStyle(.plain)
                        .font(Theme.mono)
                        .foregroundStyle(Theme.text)
                        .multilineTextAlignment(.trailing)
                        .autocorrectionDisabled()
                        .focused($editingSetup)
                        .onSubmit { Task { await save() } }
                        .padding(.horizontal, 10)
                        .frame(maxWidth: 240, minHeight: 32)
                        .background(Theme.surface, in: .rect(cornerRadius: 7))
                }
            }
            .font(.subheadline)
            .foregroundStyle(Theme.secondary)
            .disabled(!owner)
            if let error {
                Label(error, systemImage: "exclamationmark.triangle").font(.footnote).foregroundStyle(Theme.failure)
            }
            SettingsGroup(title: project.paths.count == 1 ? "Clone" : "Clones") {
                ForEach(Array(project.paths.enumerated()), id: \.offset) { index, path in
                    if index > 0 { RowDivider() }
                    HStack(spacing: 8) {
                        Text(path).font(Theme.mono).foregroundStyle(Theme.text).lineLimit(1).truncationMode(.head)
                            .textSelection(.enabled)
                        Spacer(minLength: 8)
                        CopyButton(text: path)
                    }
                    .padding(.horizontal, 14)
                    .frame(minHeight: 44)
                }
            }
            SettingsGroup(title: "Remove") {
                let live = ProjectSettingsForm.liveSessions(of: project.projectId, on: machine)
                SettingRow(label: "Remove from \(machine.name)",
                           detail: live == 0 ? "The clones stay on disk; you can add it again"
                                             : "Archive its \(live) live session\(live == 1 ? "" : "s") first") {
                    Button("Remove…", role: .destructive) { confirmingRemove = true }
                        .buttonStyle(.plain)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Theme.failure)
                        .padding(.horizontal, 12)
                        .frame(height: 32)
                        .background(Theme.raised, in: .rect(cornerRadius: 7))
                        .disabled(!owner || live > 0)
                        .opacity(owner && live == 0 ? 1 : 0.4)
                }
            }
        }
        .confirmationDialog("Remove \(project.name) from \(machine.name)?", isPresented: $confirmingRemove,
                            titleVisibility: .visible) {
            Button("Remove", role: .destructive) { Task { await remove() } }
        } message: {
            Text("Its clones stay on disk. Archived sessions keep their history.")
        }
        .onAppear {
            mode = project.defaultPermissionMode
            account = project.defaultAccount
            setup = project.setupCommand ?? ""
            loaded = true
        }
        .onChange(of: mode) { if loaded { Task { await save() } } }
        .onChange(of: account) { if loaded { Task { await save() } } }
        .onChange(of: editingSetup) { if !editingSetup { Task { await save() } } }
    }

    /// Sessions of the project on the machine that are not archived, which block removing it.
    nonisolated static func liveSessions(of projectId: ProjectId, on machine: Machine) -> Int {
        machine.sessions.filter { $0.projectId == projectId && $0.status != .archived }.count
    }

    private func remove() async {
        do {
            try await fleet.removeProject(project.projectId, on: machine.hostId)
        } catch {
            self.error = describe(error)
        }
    }

    private var accountLabel: String {
        machine.accounts.first { $0.accountId == account }?.label ?? "Most room left"
    }

    private func save() async {
        let command = setup.trimmingCharacters(in: .whitespaces)
        guard mode != project.defaultPermissionMode || account != project.defaultAccount
                || (command.isEmpty ? nil : command) != project.setupCommand else { return }
        do {
            try await fleet.setProjectSettings(project.projectId, on: machine.hostId, mode: mode, account: account,
                                               setupCommand: command.isEmpty ? nil : command)
            error = nil
        } catch {
            self.error = describe(error)
        }
    }
}

/// The hairline between rows of a settings card.
struct RowDivider: View {
    var body: some View {
        Rectangle().fill(Theme.stroke.opacity(0.6)).frame(height: 1).padding(.leading, 14)
    }
}

/// A titled card of settings rows; `RowDivider` separates them.
struct SettingsGroup<Content: View>: View {
    let title: String
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHeading(title: title)
            VStack(spacing: 0) {
                content
            }
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
            .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke.opacity(0.6)))
        }
    }
}

/// A setting: its name and what it does on the left, its control on the right.
struct SettingRow<Control: View>: View {
    let label: String
    var detail = ""
    @ViewBuilder var control: Control

    var body: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(label).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                if !detail.isEmpty { Text(detail).font(.caption).foregroundStyle(Theme.tertiary) }
            }
            Spacer(minLength: 12)
            control
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .frame(minHeight: 52)
    }
}

/// A machine: its name on this device, its connection, accounts and identity; forget it.
struct MachineSettingsSheet: View {
    let fleet: Fleet
    let hostId: HostId
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var editingAccount: Account?
    @State private var showingAccountSettings = false
    @State private var addingAccount = false
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
                Field(label: "Connection", hint: "Since the app opened. Round trips are pings every 15 seconds.") {
                    let log = fleet.connectionLog[hostId] ?? []
                    let health = ConnectionHealth(log: log, now: .now)
                    VStack(alignment: .leading, spacing: 8) {
                        HStack(spacing: 18) {
                            stat("Up for", health.currentUp.map(Self.duration) ?? "—")
                            stat("Reconnects", "\(health.reconnects)")
                            stat("Disconnected", Self.duration(health.down))
                        }
                        .padding(.bottom, 4)
                        LinkQualityView(quality: machine.quality, roundTrips: fleet.roundTrips[hostId] ?? [])
                            .padding(.bottom, 4)
                        ForEach(Array(ConnectionHealth.runs(log, now: .now).reversed().enumerated()), id: \.offset) { _, run in
                            HStack(alignment: .firstTextBaseline, spacing: 10) {
                                ConnectionMark(state: run.state)
                                Text(run.at.formatted(date: .omitted, time: .standard))
                                    .monospacedDigit().foregroundStyle(Theme.tertiary)
                                Text(run.state.label).foregroundStyle(Theme.text).lineLimit(2)
                                if run.times > 1 {
                                    Text("×\(run.times)").font(.caption.weight(.semibold)).foregroundStyle(Theme.secondary)
                                }
                                Spacer()
                                Text(Self.duration(run.lasted)).monospacedDigit().foregroundStyle(Theme.tertiary)
                            }
                            .font(.footnote)
                        }
                    }
                    .padding(12)
                    .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
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
                      hint: "A session at its limit rotates to another of the provider's accounts.") {
                    VStack(alignment: .leading, spacing: 8) {
                        if machine.accounts.isEmpty {
                            Text("No accounts yet").foregroundStyle(Theme.tertiary)
                        }
                        ForEach(machine.accounts, id: \.accountId) { account in
                            HStack {
                                Text(account.label).foregroundStyle(Theme.text)
                                Text(account.provider).foregroundStyle(Theme.secondary)
                                Spacer()
                                Text(account.accountId).font(Theme.monoSmall).foregroundStyle(Theme.tertiary)
                                if machine.role == .owner {
                                    Button("Edit") {
                                        editingAccount = account
                                        showingAccountSettings = true
                                    }.disabled(machine.connection != .connected)
                                }
                            }
                            .font(.subheadline)
                        }
                        if machine.role == .owner {
                            ActionButton(title: fleet.accountLogins[hostId] == nil ? "Add Account" : "Account Login",
                                         style: .secondary) { addingAccount = true }
                                .disabled(machine.connection != .connected)
                        } else {
                            Text("Only the machine owner can add accounts.")
                                .font(.footnote).foregroundStyle(Theme.tertiary)
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
        .sheet(isPresented: $showingAccountSettings) {
            if let editingAccount { EditAccountSheet(fleet: fleet, hostId: hostId, account: editingAccount) }
        }
        .sheet(isPresented: $addingAccount) { AddAccountSheet(fleet: fleet, hostId: hostId) }
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

    private func stat(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(value).font(.headline.monospacedDigit()).foregroundStyle(Theme.text)
            Text(label).font(.caption).foregroundStyle(Theme.tertiary)
        }
    }

    static func duration(_ seconds: TimeInterval) -> String {
        Duration.seconds(max(0, seconds)).formatted(.units(allowed: [.hours, .minutes, .seconds], width: .abbreviated, maximumUnitCount: 2))
    }
}

/// How a link performs, judged from the client core's round trips: a verdict and what to
/// look at when it is not good.
struct LinkVerdict: Equatable {
    enum Level { case good, fair, poor }
    let level: Level
    let summary: String
    var advice = ""

    init(level: Level, summary: String, advice: String = "") {
        self.level = level
        self.summary = summary
        self.advice = advice
    }

    /// `nil` until a round trip has been measured.
    init?(_ quality: ConnectionQuality) {
        guard let average = quality.averageRttMs else { return nil }
        let spread = (quality.maxRttMs ?? average) - (quality.minRttMs ?? average)
        if quality.missedPongs > 0 {
            self.init(level: .poor, summary: "\(quality.missedPongs) ping\(quality.missedPongs == 1 ? "" : "s") went unanswered",
                      advice: "The network drops packets or the machine stalls. Check its load, and its network or VPN.")
        } else if average >= 300 {
            self.init(level: .poor, summary: "Slow: \(average) ms on average",
                      advice: "Sending and streaming will lag. A direct route (not relayed) or a closer network helps.")
        } else if spread >= 200 && spread > average {
            self.init(level: .fair, summary: "Unsteady: \(quality.minRttMs ?? 0)–\(quality.maxRttMs ?? 0) ms",
                      advice: "Round trips vary a lot, often Wi-Fi or a busy uplink.")
        } else if average >= 120 {
            self.init(level: .fair, summary: "Usable: \(average) ms on average")
        } else {
            self.init(level: .good, summary: "Fast: \(average) ms on average")
        }
    }
}

/// A machine's round trips: the numbers, a verdict, and the recent ones as bars.
struct LinkQualityView: View {
    let quality: ConnectionQuality
    let roundTrips: [RoundTrip]

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 18) {
                stat("Round trip", quality.lastRttMs.map { "\($0) ms" } ?? "—")
                stat("Average", quality.averageRttMs.map { "\($0) ms" } ?? "—")
                stat("Range", quality.minRttMs.flatMap { min in quality.maxRttMs.map { "\(min)–\($0) ms" } } ?? "—")
                stat("Missed", "\(quality.missedPongs)")
            }
            if roundTrips.count > 1 { bars }
            if let verdict = LinkVerdict(quality) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Circle().fill(color(verdict.level)).frame(width: 8, height: 8)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(verdict.summary).font(.footnote.weight(.semibold)).foregroundStyle(Theme.text)
                        if !verdict.advice.isEmpty {
                            Text(verdict.advice).font(.caption).foregroundStyle(Theme.secondary)
                        }
                    }
                }
            } else {
                Text("Measuring…").font(.footnote).foregroundStyle(Theme.tertiary)
            }
        }
    }

    /// The last round trips, newest on the right, scaled to the slowest.
    private var bars: some View {
        let recent = Array(roundTrips.suffix(60))
        let top = Double(max(recent.map(\.milliseconds).max() ?? 1, 50))
        return HStack(alignment: .bottom, spacing: 2) {
            ForEach(Array(recent.enumerated()), id: \.offset) { _, trip in
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(trip.milliseconds >= 300 ? Theme.failure : trip.milliseconds >= 120 ? Theme.accent : Theme.success)
                    .frame(width: 4, height: max(2, 36 * Double(trip.milliseconds) / top))
                    .help("\(trip.milliseconds) ms at \(trip.at.formatted(date: .omitted, time: .standard))")
            }
        }
        .frame(height: 36, alignment: .bottom)
    }

    private func color(_ level: LinkVerdict.Level) -> Color {
        switch level {
        case .good: Theme.success
        case .fair: Theme.accent
        case .poor: Theme.failure
        }
    }

    private func stat(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(value).font(.headline.monospacedDigit()).foregroundStyle(Theme.text)
            Text(label).font(.caption).foregroundStyle(Theme.tertiary)
        }
    }
}

/// A machine's connection over its log: how long it has been up, how often it came back, and
/// how long it was down.
struct ConnectionHealth {
    /// A stretch of the log that kept failing the same way, or one other state.
    struct Run {
        let state: ConnectionState
        let at: Date
        var times: Int
        var lasted: TimeInterval
    }

    /// The log with retries that fail alike folded into one run: a failure and the reconnect
    /// attempt after it repeat until something else happens.
    static func runs(_ log: [ConnectionChange], now: Date) -> [Run] {
        var runs: [Run] = []
        for (index, change) in log.enumerated() {
            let lasted = (index + 1 < log.count ? log[index + 1].at : now).timeIntervalSince(change.at)
            if change.state == .connecting, index + 1 < log.count, let last = runs.last,
               last.state == log[index + 1].state, case .disconnected = last.state {
                runs[runs.count - 1].lasted += lasted
                continue
            }
            if let last = runs.last, last.state == change.state, case .disconnected = change.state {
                runs[runs.count - 1].times += 1
                runs[runs.count - 1].lasted += lasted
                continue
            }
            runs.append(Run(state: change.state, at: change.at, times: 1, lasted: lasted))
        }
        return runs
    }

    let currentUp: TimeInterval?
    let reconnects: Int
    let down: TimeInterval

    init(log: [ConnectionChange], now: Date) {
        var reconnects = 0
        var down: TimeInterval = 0
        var connectedOnce = false
        for (index, change) in log.enumerated() {
            let end = index + 1 < log.count ? log[index + 1].at : now
            if case .disconnected = change.state { down += end.timeIntervalSince(change.at) }
            if change.state == .connected {
                if connectedOnce { reconnects += 1 }
                connectedOnce = true
            }
        }
        self.reconnects = reconnects
        self.down = down
        currentUp = log.last.flatMap { $0.state == .connected ? now.timeIntervalSince($0.at) : nil }
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

/// A machine's folders, browsed through the daemon: git repositories are marked; picking one
/// sets the path.
struct FolderBrowser: View {
    let fleet: Fleet
    let hostId: HostId
    @Binding var picked: String
    @State private var path = "~"
    @State private var entries: [DirectoryEntry] = []
    @State private var error: String?
    @State private var loading = false
    /// The start of a folder name being typed, which the shown folders match.
    @State private var filter = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Button { open(parent) } label: { SwiftUI.Image(systemName: "chevron.up") }
                    .buttonStyle(.plain).foregroundStyle(Theme.secondary).disabled(path == "/")
                Text(path).font(Theme.monoSmall).foregroundStyle(Theme.secondary).lineLimit(1).truncationMode(.head)
                Spacer()
                if loading { ProgressView().controlSize(.mini).tint(Theme.tertiary) }
            }
            .padding(.horizontal, 10)
            .frame(height: 30)
            Rectangle().fill(Theme.stroke).frame(height: 1)
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure).padding(10)
                Spacer()
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(entries.filter { $0.isDir && (filter.isEmpty || $0.name.lowercased().hasPrefix(filter)) },
                                id: \.name) { entry in
                            let full = join(path, entry.name)
                            Button {
                                if entry.isRepo { picked = full } else { open(full) }
                            } label: {
                                HStack(spacing: 8) {
                                    SwiftUI.Image(systemName: entry.isRepo ? "shippingbox.fill" : "folder")
                                        .foregroundStyle(entry.isRepo ? Theme.accent : Theme.secondary)
                                        .frame(width: 18)
                                    Text(entry.name).foregroundStyle(Theme.text)
                                    if entry.isRepo { Text("repository").font(.caption).foregroundStyle(Theme.tertiary) }
                                    Spacer()
                                    if picked == full { SwiftUI.Image(systemName: "checkmark").foregroundStyle(Theme.text) }
                                    if entry.isRepo {
                                        Button { open(full) } label: { SwiftUI.Image(systemName: "chevron.right") }
                                            .buttonStyle(.plain).foregroundStyle(Theme.tertiary).help("Open the folder")
                                    }
                                }
                                .font(.subheadline)
                                .padding(.horizontal, 10)
                                .frame(height: 32)
                                .background(picked == full ? Theme.raised : .clear)
                                .contentShape(.rect)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
            }
        }
        .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        .overlay(RoundedRectangle(cornerRadius: Theme.corner).strokeBorder(Theme.stroke))
        .task { await load(picked.isEmpty ? "~" : picked) }
        // Typing browses: a path ending in "/" opens that folder; otherwise its folder is
        // shown, filtered to names starting with what follows the last "/".
        .task(id: picked) {
            let typed = picked.trimmingCharacters(in: .whitespaces)
            guard typed.hasPrefix("/") || typed.hasPrefix("~"), typed != path, !isEntry(typed) else {
                filter = ""
                return
            }
            let (folder, start) = FolderBrowser.split(typed)
            filter = start.lowercased()
            guard folder != path else { return }
            try? await Task.sleep(for: .milliseconds(300))
            await load(folder, quiet: true)
        }
    }

    /// A typed path as the folder to list and the start of a name in it.
    static func split(_ typed: String) -> (folder: String, start: String) {
        if typed == "~" { return ("~", "") }
        if typed.hasSuffix("/") { return (typed.count > 1 ? String(typed.dropLast()) : typed, "") }
        guard let slash = typed.lastIndex(of: "/") else { return (typed, "") }
        let folder = slash == typed.startIndex ? "/" : String(typed[..<slash])
        return (folder, String(typed[typed.index(after: slash)...]))
    }

    /// Whether the path is one of the shown folders, picked rather than typed.
    private func isEntry(_ full: String) -> Bool {
        entries.contains { join(path, $0.name) == full }
    }

    private var parent: String {
        let trimmed = path.hasSuffix("/") && path.count > 1 ? String(path.dropLast()) : path
        guard let slash = trimmed.lastIndex(of: "/") else { return "/" }
        return slash == trimmed.startIndex ? "/" : String(trimmed[..<slash])
    }

    private func join(_ base: String, _ name: String) -> String {
        base.hasSuffix("/") ? base + name : base + "/" + name
    }

    private func open(_ next: String) {
        Task { await load(next) }
    }

    /// Lists a folder and makes it the path; `quiet` keeps the last listing when a half-typed
    /// path does not exist.
    private func load(_ next: String, quiet: Bool = false) async {
        loading = true
        defer { loading = false }
        do {
            let listing = try await fleet.listDirectory(next, on: hostId)
            path = listing.path
            entries = listing.entries.sorted { ($0.isRepo ? 0 : 1, $0.name.lowercased()) < ($1.isRepo ? 0 : 1, $1.name.lowercased()) }
            error = nil
            if !quiet { picked = listing.path }
        } catch {
            if !quiet { self.error = describe(error) }
        }
    }
}
