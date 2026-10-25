import Herder
import SwiftUI

/// A project's settings page, on each machine that has it, editable by the machine's owners: its
/// name and icon, the permissions and account new sessions start with, and the command a new
/// worktree runs first. Changes apply as they are made.
struct ProjectSettingsPage: View {
    let fleet: Fleet
    let projectId: String
    @State private var addingMachine = false
    @State private var hostId: HostId?
    @Environment(\.pages) private var pages

    var body: some View {
        let group = fleet.lists.projects.first { $0.id == projectId }
        let machines = fleet.machines.filter { $0.projects.contains { $0.projectId == projectId } }
        let machine = machines.first { $0.hostId == hostId } ?? machines.first
        DetailPage(title: group?.name ?? "Project", status: status(machines, shown: machine)) {
            ProjectIcon(projectId: group?.projectId, name: group?.name, image: fleet.projectIcon(group?.projectId),
                        size: 34)
        } actions: {
            PaneButton(title: "Add to Machine", symbol: "plus") { addingMachine = true }
        } content: {
            if machines.count > 1 {
                MachineTabs(machines: machines, selection: Binding(get: { machine?.hostId }, set: { hostId = $0 }))
            }
            if let machine, let project = machine.projects.first(where: { $0.projectId == projectId }) {
                ProjectSettingsForm(fleet: fleet, machine: machine, project: project)
                    .id(machine.hostId)
            } else {
                Text("No connected machine has this project.").foregroundStyle(Theme.secondary)
            }
            ProjectReachGroup(fleet: fleet, reach: ProjectReach(projectId: projectId, machines: fleet.machines))
        }
        .sheet(isPresented: $addingMachine) {
            ProjectPicker(fleet: fleet, newProject: true, picked: { selection in
                hostId = selection.hostId
                addingMachine = false
            }, existingProjectId: projectId)
        }
        // Removed from its last machine: nothing is left to set.
        .onChange(of: machines.isEmpty) { if machines.isEmpty { pages.close() } }
    }

    /// "On 2 machines · github.com/acme/web", and who may change the shown machine's settings.
    private func status(_ machines: [Machine], shown: Machine?) -> String {
        var parts = [machines.count == 1 ? "On 1 machine" : "On \(machines.count) machines", projectId]
        if let shown, shown.role != .owner { parts.append("only \(shown.name)’s owners can change these") }
        return parts.joined(separator: " · ")
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
    @State private var name = ""
    @State private var mode: PermissionMode?
    @State private var account: AccountId?
    @State private var setup = ""
    @State private var iconBackground: String?
    @State private var privateArtifacts = false
    @State private var loaded = false
    @State private var error: String?
    @State private var confirmingRemove = false
    @State private var saves = Autosave()
    @FocusState private var editingSetup: Bool
    @FocusState private var editingName: Bool

    var body: some View {
        let owner = machine.role == .owner
        VStack(alignment: .leading, spacing: 18) {
            SettingsList(title: "Appearance") {
                SettingRow(label: "Name", hint: "Blank goes back to the repository's", saved: saves["Name"]) {
                    TextField(defaultName, text: $name)
                        .textFieldStyle(.plain)
                        .foregroundStyle(Theme.text)
                        .multilineTextAlignment(.trailing)
                        .autocorrectionDisabled()
                        .focused($editingName)
                        .onSubmit { saveAppearance("Name") }
                        .padding(.horizontal, 10)
                        .frame(maxWidth: 240, minHeight: 32)
                        .background(Theme.surface, in: .rect(cornerRadius: 7))
                }
                RowDivider()
                ProjectIconRow(fleet: fleet, project: project, background: iconBackground,
                               error: $error)
                RowDivider()
                SettingRow(label: "Background", hint: "Fills the tile behind the icon", saved: saves["Background"]) {
                    FooterItem(symbol: "paintpalette", text: Self.backgroundLabel(iconBackground)) {
                        Picker("Background", selection: $iconBackground) {
                            ForEach(Self.backgrounds, id: \.colour) { Text($0.label).tag($0.colour) }
                            if let iconBackground, !Self.backgrounds.contains(where: { $0.colour == iconBackground }) {
                                Text(iconBackground).tag(Optional(iconBackground))
                            }
                        }
                        .pickerStyle(.inline)
                    }
                }
                .font(.subheadline)
                .foregroundStyle(Theme.secondary)
            }
            .disabled(!owner)
            .detailSection("Appearance")
            SettingsList(title: "New sessions") {
                SettingRow(label: "Permissions", hint: "What agents may do without asking", saved: saves["Permissions"]) {
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
                SettingRow(label: "Account", hint: "Rotates to another when it runs out", saved: saves["Account"]) {
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
                SettingRow(label: "Setup command", hint: "Runs once in each new worktree", saved: saves["Setup command"]) {
                    TextField("make bootstrap", text: $setup)
                        .textFieldStyle(.plain)
                        .font(Theme.mono)
                        .foregroundStyle(Theme.text)
                        .multilineTextAlignment(.trailing)
                        .autocorrectionDisabled()
                        .focused($editingSetup)
                        .onSubmit { save("Setup command") }
                        .padding(.horizontal, 10)
                        .frame(maxWidth: 240, minHeight: 32)
                        .background(Theme.surface, in: .rect(cornerRadius: 7))
                }
            }
            .font(.subheadline)
            .foregroundStyle(Theme.secondary)
            .disabled(!owner)
            .detailSection("New sessions")
            SettingsList(title: "Artifacts") {
                SettingRow(label: "Keep private", hint: "Shown in the thread, never uploaded for a public link",
                           saved: saves["Keep private"]) {
                    Toggle("Keep private", isOn: $privateArtifacts)
                        .labelsHidden()
                        .toggleStyle(.switch)
                        .accessibilityIdentifier("private-artifacts")
                }
            }
            .font(.subheadline)
            .foregroundStyle(Theme.secondary)
            .disabled(!owner)
            .detailSection("Artifacts")
            if let error {
                Label(error, systemImage: "exclamationmark.triangle").font(.footnote).foregroundStyle(Theme.failure)
            }
            SettingsList(title: project.paths.count == 1 ? "Clone" : "Clones") {
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
            .detailSection("Clones")
            DangerZone {
                let live = ProjectSettingsForm.liveSessions(of: project.projectId, on: machine)
                SettingRow(label: "Remove from \(machine.name)",
                           hint: live == 0 ? "The clones stay on disk; you can add it again"
                                             : "Archive its \(live) live session\(live == 1 ? "" : "s") first") {
                    DangerButton(title: "Remove…") { confirmingRemove = true }
                        .disabled(!owner || live > 0)
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
            name = project.name
            mode = project.defaultPermissionMode
            account = project.defaultAccount
            setup = project.setupCommand ?? ""
            iconBackground = project.iconBackground
            privateArtifacts = project.privateArtifacts
            loaded = true
        }
        .onChange(of: mode) { if loaded { save("Permissions") } }
        .onChange(of: iconBackground) { if loaded { saveAppearance("Background") } }
        .onChange(of: account) { if loaded { save("Account") } }
        .onChange(of: privateArtifacts) { if loaded { save("Keep private") } }
        .onChange(of: editingSetup) { if !editingSetup { save("Setup command") } }
        .onChange(of: editingName) { if !editingName { saveAppearance("Name") } }
    }

    /// The icon backgrounds to choose from; any other `#rrggbb` comes from the config file.
    static let backgrounds: [(label: String, colour: String?)] = [
        ("None", nil), ("White", "#ffffff"), ("Light grey", "#e5e5e5"), ("Dark grey", "#2a2a2a"), ("Black", "#000000"),
    ]

    static func backgroundLabel(_ colour: String?) -> String {
        backgrounds.first { $0.colour == colour }?.label ?? colour ?? "None"
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

    /// The name the machine gives the project when none is set: the last segment of its id.
    private var defaultName: String {
        project.projectId.split(whereSeparator: { $0 == "/" || $0 == ":" }).last.map(String.init) ?? project.projectId
    }

    /// Saves the machine's settings for the project after a change to `row`, unless nothing changed.
    private func save(_ row: String) {
        let command = setup.trimmingCharacters(in: .whitespaces)
        guard mode != project.defaultPermissionMode || account != project.defaultAccount
                || (command.isEmpty ? nil : command) != project.setupCommand
                || privateArtifacts != project.privateArtifacts else { return }
        Task {
            await saves.save(row) {
                try await fleet.setProjectSettings(project.projectId, on: machine.hostId, name: appearanceName,
                                                   mode: mode, account: account,
                                                   setupCommand: command.isEmpty ? nil : command,
                                                   iconBackground: iconBackground, privateArtifacts: privateArtifacts)
            }
        }
    }

    /// The name as the machines get it: blank goes back to the repository's.
    private var appearanceName: String {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty ? defaultName : trimmed
    }

    /// The name and background go to every machine, as the icon does, so all devices show them.
    private func saveAppearance(_ row: String) {
        guard appearanceName != project.name || iconBackground != project.iconBackground else { return }
        Task {
            await saves.save(row) {
                try await fleet.setProjectAppearance(project.projectId, name: appearanceName, background: iconBackground)
            }
        }
    }
}
