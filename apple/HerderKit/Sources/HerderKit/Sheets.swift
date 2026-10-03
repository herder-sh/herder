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

/// A project's settings on each machine that has it, editable by the machine's owners: the
/// permissions and account new sessions start with, and the command a new worktree runs first.
struct ProjectSettingsSheet: View {
    let fleet: Fleet
    let projectId: String

    var body: some View {
        let group = fleet.lists.projects.first { $0.id == projectId }
        SheetScaffold(title: group?.name ?? "Project", subtitle: projectId) {
            ForEach(fleet.machines.filter { $0.projects.contains { $0.projectId == projectId } }, id: \.hostId) { machine in
                if let project = machine.projects.first(where: { $0.projectId == projectId }) {
                    ProjectSettingsForm(fleet: fleet, machine: machine, project: project)
                }
            }
        } footer: {
            Spacer()
        }
    }
}

private struct ProjectSettingsForm: View {
    let fleet: Fleet
    let machine: Machine
    let project: Project
    @State private var mode: PermissionMode?
    @State private var account: AccountId?
    @State private var setup = ""
    @State private var saved = false
    @State private var error: String?

    var body: some View {
        let owner = machine.role == .owner
        Field(label: machine.name) {
            VStack(alignment: .leading, spacing: 14) {
                DetailRow(label: "Clones", value: project.paths.joined(separator: "\n"), mono: true)
                VStack(alignment: .leading, spacing: 6) {
                    Text("New sessions start with").font(.caption).foregroundStyle(Theme.secondary)
                    ChoiceChips(options: [(PermissionMode?.none, "Ask each time", "")]
                                + [PermissionMode.readOnly, .ask, .autoEdit, .fullAccess].map { (Optional($0), $0.label, "") },
                                selection: $mode)
                }
                VStack(alignment: .leading, spacing: 6) {
                    Text("Default account").font(.caption).foregroundStyle(Theme.secondary)
                    ChoiceChips(options: [(AccountId?.none, "Most room left", "")]
                                + machine.accounts.map { (Optional($0.accountId), $0.label, $0.provider) },
                                selection: $account)
                }
                VStack(alignment: .leading, spacing: 6) {
                    Text("Setup command, run once in each new worktree").font(.caption).foregroundStyle(Theme.secondary)
                    InputBox(placeholder: "make bootstrap", text: $setup, mono: true)
                }
                HStack {
                    if let error { Text(error).font(.footnote).foregroundStyle(Theme.failure) }
                    if saved { Label("Saved", systemImage: "checkmark").font(.footnote).foregroundStyle(Theme.secondary) }
                    Spacer()
                    ActionButton(title: "Save", style: .primary) { await save() }
                        .frame(width: 120)
                        .disabled(!owner)
                        .opacity(owner ? 1 : 0.4)
                }
                if !owner {
                    Text("Only the machine's owners can change these.").font(.footnote).foregroundStyle(Theme.tertiary)
                }
            }
            .padding(12)
            .background(Theme.background, in: .rect(cornerRadius: Theme.corner))
        }
        .onAppear {
            mode = project.defaultPermissionMode
            account = project.defaultAccount
            setup = project.setupCommand ?? ""
        }
    }

    private func save() async {
        let command = setup.trimmingCharacters(in: .whitespaces)
        do {
            try await fleet.setProjectSettings(project.projectId, on: machine.hostId, mode: mode, account: account,
                                               setupCommand: command.isEmpty ? nil : command)
            saved = true
            error = nil
        } catch {
            self.error = describe(error)
            saved = false
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
                Field(label: "Connection", hint: "Since the app opened. Round-trip times need the client core to report them (proposed in P0.11).") {
                    let log = fleet.connectionLog[hostId] ?? []
                    let health = ConnectionHealth(log: log, now: .now)
                    VStack(alignment: .leading, spacing: 8) {
                        HStack(spacing: 18) {
                            stat("Up for", health.currentUp.map(Self.duration) ?? "—")
                            stat("Reconnects", "\(health.reconnects)")
                            stat("Disconnected", Self.duration(health.down))
                        }
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
                      hint: "Accounts are set in the machine's `daemon.toml`; a session at its limit rotates to another of the provider's accounts.") {
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
