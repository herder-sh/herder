import Herder
import SwiftUI

/// A project's settings page: its name and icon, the permissions, account and setup command new
/// sessions start with on each machine, where it is cloned and where it could be, and removing it
/// from a machine. Each machine's owners change its settings; changes apply as they are made.
struct ProjectSettingsPage: View {
    let fleet: Fleet
    let projectId: String
    @State private var addingMachine = false
    @State private var hostId: HostId?
    /// The machines a clone is running on.
    @State private var cloning: Set<HostId> = []
    @State private var removing: ProjectReach.Clone?
    @State private var error: String?
    @Environment(\.pages) private var pages

    var body: some View {
        let group = fleet.lists.projects.first { $0.id == projectId }
        let machines = fleet.machines.filter { $0.projects.contains { $0.projectId == projectId } }
        let machine = machines.first { $0.hostId == hostId } ?? machines.first
        let reach = ProjectReach(projectId: projectId, machines: fleet.machines)
        DetailPage(title: group?.name ?? "Project", status: status(machines, shown: machine)) {
            ProjectIcon(projectId: group?.projectId, name: group?.name, image: fleet.projectIcon(group?.projectId),
                        size: 34)
        } actions: {
            PaneButton(title: "Add to Machine", symbol: "plus") { addingMachine = true }
        } content: {
            if let machine, let project = machine.projects.first(where: { $0.projectId == projectId }) {
                ProjectSettingsForm(fleet: fleet, machine: machine, project: project, machines: machines,
                                    shown: Binding(get: { machine.hostId }, set: { hostId = $0 }))
                    .id(machine.hostId)
            } else {
                Text("No connected machine has this project.").foregroundStyle(Theme.secondary)
            }
            machinesList(reach)
            skillsLink
            if let error {
                Label(error, systemImage: "exclamationmark.triangle").font(.footnote).foregroundStyle(Theme.failure)
            }
            if !reach.clones.isEmpty {
                dangerZone(reach.clones)
            }
        }
        .sheet(isPresented: $addingMachine) {
            ProjectPicker(fleet: fleet, newProject: true, picked: { selection in
                hostId = selection.hostId
                addingMachine = false
            }, existingProjectId: projectId)
        }
        .confirmationDialog("Remove \(group?.name ?? "the project") from \(removing?.name ?? "")?",
                            isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
                            titleVisibility: .visible, presenting: removing) { clone in
            Button("Remove", role: .destructive) { Task { await remove(from: clone.hostId) } }
        } message: { _ in
            Text("Its clones stay on disk. Archived sessions keep their history.")
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

    /// Each machine with its clones and sessions, then each it could be cloned onto.
    private func machinesList(_ reach: ProjectReach) -> some View {
        SettingsList(title: "Machines") {
            ForEach(Array(reach.clones.enumerated()), id: \.element.id) { index, clone in
                if index > 0 { RowDivider() }
                HStack(spacing: 8) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(clone.name).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                        ForEach(clone.paths, id: \.self) { path in
                            Text(path).font(.caption.monospaced()).foregroundStyle(Theme.tertiary)
                                .lineLimit(1).truncationMode(.head).textSelection(.enabled)
                        }
                    }
                    Spacer(minLength: 8)
                    Text(clone.sessions == 1 ? "1 session" : "\(clone.sessions) sessions")
                        .font(.caption).foregroundStyle(Theme.secondary)
                    if let path = clone.paths.first { CopyButton(text: path) }
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
                .frame(minHeight: 52)
            }
            if let remote = reach.remote {
                ForEach(Array(reach.missing.enumerated()), id: \.element.id) { index, machine in
                    if index > 0 || !reach.clones.isEmpty { RowDivider() }
                    SettingRow(label: machine.name, hint: detail(machine, remote: remote)) {
                        if cloning.contains(machine.hostId) {
                            ProgressView().controlSize(.small)
                        } else {
                            PillButton(title: "Clone") { Task { await clone(remote, on: machine.hostId) } }
                                .disabled(!machine.owner || !machine.connected)
                        }
                    }
                }
            }
        }
        .detailSection("Machines")
    }

    private func detail(_ machine: ProjectReach.Missing, remote: String) -> String {
        if cloning.contains(machine.hostId) { return "Cloning \(remote)…" }
        if !machine.owner { return "Not cloned · only its owners can clone it there" }
        if !machine.connected { return "Not cloned · connect to it to clone it there" }
        return "Not cloned · clones \(remote) into its projects folder"
    }

    private func clone(_ remote: String, on hostId: HostId) async {
        cloning.insert(hostId)
        defer { cloning.remove(hostId) }
        do {
            _ = try await fleet.cloneProject(remote, into: nil, on: hostId)
            error = nil
        } catch {
            self.error = describe(error)
        }
    }

    /// The skills the project checks in, and the way to the Skills screen, where the library's
    /// skills and MCP servers are turned on per machine.
    private var skillsLink: some View {
        let count = ProjectSkills.all(fleet.machines).first { $0.projectId == projectId }?.skills.count ?? 0
        return SettingsList(title: "MCP servers & skills") {
            Button { pages.open(.skills) } label: {
                SettingRow(label: count == 1 ? "1 checked-in skill" : "\(count) checked-in skills",
                           hint: "The library and MCP servers are set per machine") {
                    Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.tertiary)
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("project-skills-link")
        }
        .detailSection("MCP servers & skills")
    }

    /// Removing the project from one of its machines, once its sessions there are archived.
    private func dangerZone(_ clones: [ProjectReach.Clone]) -> some View {
        DangerZone {
            SettingRow(label: "Remove from a machine", hint: "The clones stay on disk; you can add it again") {
                Menu {
                    ForEach(clones) { clone in
                        Button(clone.live == 0 ? clone.name
                               : "\(clone.name) · archive its \(clone.live) live session\(clone.live == 1 ? "" : "s") first") {
                            removing = clone
                        }
                        .disabled(!clone.owner || clone.live > 0)
                    }
                } label: {
                    Text("Remove…")
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Theme.danger)
                        .padding(.horizontal, 12)
                        .frame(height: 32)
                        .background(Theme.raised, in: .rect(cornerRadius: 7))
                }
                .menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden)
                .fixedSize()
                .disabled(!clones.contains { $0.owner })
            }
        }
    }

    private func remove(from hostId: HostId) async {
        do {
            try await fleet.removeProject(projectId, on: hostId)
            error = nil
        } catch {
            self.error = describe(error)
        }
    }
}

/// The project's appearance, shared by every machine, and the settings new sessions start with
/// on the shown one of `machines`.
struct ProjectSettingsForm: View {
    let fleet: Fleet
    let machine: Machine
    let project: Project
    let machines: [Machine]
    @Binding var shown: HostId
    @State private var name = ""
    @State private var mode: PermissionMode?
    @State private var account: AccountId?
    @State private var setup = ""
    @State private var iconBackground: String?
    @State private var privateArtifacts = false
    @State private var loaded = false
    @State private var error: String?
    @State private var saves = Autosave()
    @FocusState private var editingSetup: Bool
    @FocusState private var editingName: Bool

    var body: some View {
        let owner = machine.role == .owner
        VStack(alignment: .leading, spacing: 18) {
            SettingsList(title: "Appearance") {
                ProjectIconRow(fleet: fleet, project: project, background: iconBackground,
                               error: $error)
                RowDivider()
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
                if machines.count > 1 {
                    SettingRow(label: "On", hint: "Each machine keeps its own") {
                        FooterItem(symbol: "desktopcomputer", text: machine.name) {
                            Picker("Machine", selection: $shown) {
                                ForEach(machines, id: \.hostId) { Text($0.name).tag($0.hostId) }
                            }
                            .pickerStyle(.inline)
                        }
                    }
                    .accessibilityIdentifier("project-settings-machine")
                    RowDivider()
                }
                Group {
                    SettingRow(label: "Permissions", hint: "What agents may do without asking", saved: saves["Permissions"]) {
                        // No mode set starts sessions asking, as Ask does. A menu where the four
                        // segments do not fit, as on a phone.
                        let selection = Binding(get: { mode ?? .ask }, set: { mode = $0 })
                        let modes = [PermissionMode.readOnly, .ask, .autoEdit, .fullAccess]
                        ViewThatFits(in: .horizontal) {
                            Picker("Permissions", selection: selection) {
                                ForEach(modes, id: \.self) { Text($0.label).tag($0) }
                            }
                            .pickerStyle(.segmented)
                            .labelsHidden()
                            .fixedSize()
                            FooterItem(symbol: "lock.shield", text: (mode ?? .ask).label) {
                                Picker("Permissions", selection: selection) {
                                    ForEach(modes, id: \.self) { Text($0.label).tag($0) }
                                }
                                .pickerStyle(.inline)
                            }
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
                    RowDivider()
                    SettingRow(label: "Keep artifacts private", hint: "Shown in the thread, never uploaded for a public link",
                               saved: saves["Keep private"]) {
                        Toggle("Keep artifacts private", isOn: $privateArtifacts)
                            .labelsHidden()
                            .toggleStyle(.settings)
                            .accessibilityIdentifier("private-artifacts")
                    }
                }
                .disabled(!owner)
            }
            .font(.subheadline)
            .foregroundStyle(Theme.secondary)
            .detailSection("New sessions")
            if let error {
                Label(error, systemImage: "exclamationmark.triangle").font(.footnote).foregroundStyle(Theme.failure)
            }
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
