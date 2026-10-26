import Herder
import SwiftUI
#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// The sheets the app presents: short create flows only. Objects open as pages (`Page`).
enum AppSheet: Identifiable, Hashable {
    case pair
    /// Add Machine with a `herder://pair` link the app was opened with already filled in.
    case pairLink(String)
    /// A link that pairs another device with this one's machines.
    case share
    /// A new session: pick its project first.
    case newSession
    /// A new project: a first session in a repository herder does not list yet.
    case newProject
    /// A new provider account, on as many machines as are ticked.
    case addAccount
    /// Signs every owned machine in to AWS IAM Identity Center.
    case awsSignIn
    /// The installs, logins and sign-ins running, as a sheet on the iPhone.
    case activity
    /// A new skill, or one of the library's to edit.
    case skill(SkillEditing)
    /// A skill to import into the library.
    case importSkill
    /// A new chat bot: pick its machine first.
    case addBot

    var id: Self { self }
}

extension AppSheet {
    @MainActor @ViewBuilder
    func view(fleet: Fleet, drafted: @escaping (Draft) -> Void) -> some View {
        switch self {
        case .pair: PairSheet(fleet: fleet)
        case .pairLink(let link): PairSheet(fleet: fleet, link: link)
        case .share: ShareSheet(fleet: fleet)
        case .newSession: ProjectPicker(fleet: fleet, newProject: false, picked: drafted)
        case .newProject: ProjectPicker(fleet: fleet, newProject: true, picked: drafted)
        case .addAccount: AddAccountSheet(fleet: fleet)
        case .awsSignIn: AwsSignInSheet(fleet: fleet)
        case .activity: ActivitySheet(fleet: fleet)
        case .skill(let editing): SkillEditorSheet(fleet: fleet, editing: editing)
        case .importSkill: SkillImportSheet(fleet: fleet)
        case .addBot: AddBotSheet(fleet: fleet)
        }
    }
}

/// Pairs with the machines a `herder://pair` link names: the one `herder pair` prints, or one
/// another device shares for all of its machines.
struct PairSheet: View {
    let fleet: Fleet
    @Environment(\.dismiss) private var dismiss
    @State private var link: String
    @State private var error: String?
    @State private var scanning = false
    /// Each machine's result, once a link of several machines, or one that failed, paired.
    @State private var results: [PairResult]?

    init(fleet: Fleet, link: String = "") {
        self.fleet = fleet
        _link = State(initialValue: link)
    }

    var body: some View {
        #if os(iOS)
        // The camera takes the sheet's place rather than opening over it.
        if scanning {
            PairScanner(scanned: { link = $0 }, close: { scanning = false })
                .presentationBackground(.black)
        } else {
            form
        }
        #else
        form
        #endif
    }

    private var form: some View {
        SheetScaffold(title: "Add Machine", subtitle: "Pair this device with a machine running herder.",
                      height: machines == nil ? 420 : 560,
                      hint: results != nil ? "" : machines == nil ? "Or, on a device already paired, open Machines › Pair Another Device."
                          : "A fingerprint must match the one `herder pair` printed.") {
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
        Field(label: scanLabel) {
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
            Field(label: machines.count == 1 ? "3 · Check it is your machine" : "3 · Check they are your \(machines.count) machines") {
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
func grouped(_ fingerprint: String) -> String {
    stride(from: 0, to: fingerprint.count, by: 4).map { start in
        let from = fingerprint.index(fingerprint.startIndex, offsetBy: start)
        return String(fingerprint[from..<(fingerprint.index(from, offsetBy: 4, limitedBy: fingerprint.endIndex) ?? fingerprint.endIndex)])
    }
    .joined(separator: " ")
}

/// A session about to start: where it runs. The chat opens empty, with its account, model
/// and permissions preselected; the first prompt creates it.
struct Draft: Codable, Hashable, Identifiable {
    var hostId: HostId
    /// The project to start in, or `nil` for `repo`; neither for a chat.
    var projectId: String?
    var repo: String?
    var id: String { "\(hostId)/\(key)" }
    /// What the draft is kept on this device by: its project or path, on whichever machine;
    /// one key for every chat.
    var key: String { projectId ?? repo ?? Self.chatKey }
    /// Whether it starts a chat, a session about no project.
    var isChat: Bool { projectId == nil && repo == nil }

    /// What a chat draft is kept by; no project id or path is ever this.
    static let chatKey = "chat"

    /// What creating its session names: the project when known, else the path; the daemon
    /// takes exactly one. A just-added project keeps its path only to show it.
    var createArguments: (repo: String?, projectId: String?) {
        projectId == nil ? (repo, nil) : (nil, projectId)
    }

    /// A chat on the connected machine, not a vault, last picked for a chat on this device, else
    /// the first.
    @MainActor
    static func chat(fleet: Fleet) -> Draft? {
        let candidates = fleet.machines.filter { $0.connection == .connected && $0.hosts.isEmpty }.map(\.hostId)
        return machine(among: candidates, last: MachinePreference.last(for: chatKey), newest: nil)
            .map { Draft(hostId: $0) }
    }

    /// A draft in a project, on the connected machine that has it that `machine` picks.
    @MainActor
    static func inProject(_ projectId: String, fleet: Fleet) -> Draft? {
        let candidates = fleet.machines
            .filter { $0.connection == .connected && $0.projects.contains { $0.projectId == projectId } }
        return machine(for: projectId, among: candidates.map(\.hostId), fleet: fleet)
            .map { Draft(hostId: $0, projectId: projectId) }
    }

    /// Where a new session in a project starts, of `candidates`: the machine last picked for a
    /// new session in the project on this device, else the one the project's newest session ran on, else the
    /// first.
    @MainActor
    static func machine(for projectId: String, among candidates: [HostId], fleet: Fleet) -> HostId? {
        let newest = fleet.lists.projects.first { $0.projectId == projectId }?.sessions
            .compactMap { session in fleet.session(session.key).map { (session.key.hostId, $0.updatedAt ?? .distantPast) } }
            .max { $0.1 < $1.1 }?.0
        return machine(among: candidates, last: MachinePreference.last(for: projectId), newest: newest)
    }

    static func machine(among candidates: [HostId], last: HostId?, newest: HostId?) -> HostId? {
        [last, newest].compactMap { $0 }.first(where: candidates.contains) ?? candidates.first
    }
}

/// Picks where a new session runs, as a palette: one row per project, across machines, the most
/// recently used first, then a repository on a machine for a new project: one there already, or
/// one the machine clones. The chat opens on the machine last picked for a new session in the
/// project, else the one the project was used on last; it can change there.
struct ProjectPicker: View {
    let fleet: Fleet
    let newProject: Bool
    let picked: (Draft) -> Void
    var existingProjectId: String? = nil
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var other = false
    @State private var hostId: HostId = ""
    @State private var input = ProjectRepositoryInput()
    @State private var submitting = false
    @FocusState private var searching: Bool

    /// Machines that run sessions: connected, and not a vault.
    private var machines: [Machine] {
        fleet.machines.filter { machine in
            machine.connection == .connected && machine.hosts.isEmpty
                && !machine.projects.contains { $0.projectId == existingProjectId }
        }
    }

    typealias Row = (id: String, name: String, machines: [Machine])

    /// Each project once, with the machines that have it.
    private var projects: [Row] {
        var order: [String] = []
        var groups: [String: (name: String, machines: [Machine])] = [:]
        for machine in machines {
            for project in machine.projects {
                if groups[project.projectId] == nil { order.append(project.projectId) }
                groups[project.projectId, default: (project.name, [])].machines.append(machine)
            }
        }
        let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
        let rows: [Row] = order.compactMap { id in groups[id].map { (id, $0.name, $0.machines) } }
            .filter { needle.isEmpty || $0.name.lowercased().contains(needle) }
        return Self.ranked(rows, by: fleet.lists.projects)
    }

    /// `rows` with the project a session last did something in first, as `groups` rank them;
    /// one no group ranks goes last, by name.
    nonisolated static func ranked(_ rows: [Row], by groups: [ProjectGroup]) -> [Row] {
        let recency = Dictionary(groups.map { ($0.id, $0.recency) }) { first, _ in first }
        return rows.sorted { a, b in
            (recency[a.id] ?? .max, a.name.lowercased()) < (recency[b.id] ?? .max, b.name.lowercased())
        }
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
            searching = !newProject
        }
        .onChange(of: hostId) {
            input.changeMachine()
        }
        // Clones go into the machine's own projects folder unless moved; owners only, as cloning is.
        .task(id: hostId) {
            guard !hostId.isEmpty, let dir = try? await fleet.projectsDir(on: hostId) else { return }
            input.setProjectsDir(dir)
        }
        .onChange(of: input.cloning) { input.addError = nil }
        .onChange(of: input.repo) { input.addError = nil }
        .onChange(of: input.url) { input.addError = nil }
        .onChange(of: input.into) { input.addError = nil }
    }

    /// Choose the machine first; repository paths always belong to that machine.
    private var pathForm: some View {
        VStack(alignment: .leading, spacing: 20) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Add project").font(.title3.weight(.semibold)).foregroundStyle(Theme.text)
                    Text("Choose a repository on the machine where you’ll work.")
                        .font(.subheadline).foregroundStyle(Theme.secondary)
                }
                Spacer()
                Button { dismiss() } label: { Image(systemName: "xmark") }
                    .buttonStyle(.plain).foregroundStyle(Theme.secondary)
                    .accessibilityLabel("Cancel")
                    .disabled(submitting)
            }
            if machines.isEmpty {
                Text(existingProjectId == nil ? "Connect a machine to add a project." : "No other connected machine is available for this project.")
                    .font(.subheadline).foregroundStyle(Theme.secondary)
            } else {
                HStack {
                    Label("Machine", systemImage: "desktopcomputer")
                        .font(.subheadline).foregroundStyle(Theme.secondary)
                    Spacer()
                    Picker("Machine", selection: $hostId) {
                        ForEach(machines, id: \.hostId) { machine in
                            Text(machine.name).tag(machine.hostId)
                        }
                    }
                    .labelsHidden().pickerStyle(.menu)
                }
                .disabled(submitting)
                Picker("Repository source", selection: $input.cloning) {
                    Text("Use existing").tag(false)
                    Text("Clone repository").tag(true)
                }
                .pickerStyle(.segmented)
                .disabled(submitting)
                repositoryFields.disabled(submitting)
            }
            if let addError = input.addError {
                VStack(alignment: .leading, spacing: 8) {
                    Label(addError, systemImage: "exclamationmark.circle")
                        .font(.footnote).foregroundStyle(Theme.failure)
                    if input.cloning {
                        Text("If this folder already contains your repository, use it instead.")
                            .font(.footnote).foregroundStyle(Theme.secondary)
                        Button("Use existing repository") {
                            input.useExistingRepository()
                        }
                        .disabled(submitting)
                    }
                }
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Theme.raised, in: .rect(cornerRadius: Theme.corner))
            }
            HStack {
                Button("Cancel") { dismiss() }.disabled(submitting)
                Spacer()
                ActionButton(title: input.cloning ? "Clone and add" : "Add project", style: .primary) {
                    await submitPath()
                }
                .frame(width: 160)
                .disabled(!pathReady || submitting)
                .opacity(pathReady ? 1 : 0.4)
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(24)
        .interactiveDismissDisabled(submitting)
    }

    @ViewBuilder private var repositoryFields: some View {
        if input.cloning {
            Field(label: "Repository", hint: "GitHub owner/repo or a git URL. Uses this machine’s git login.") {
                InputBox(placeholder: "owner/repo", text: $input.url, mono: true)
                    .onChange(of: input.url) { old, new in
                        input.updateCloneDestination(from: old, to: new)
                    }
            }
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text("Location").font(.subheadline).foregroundStyle(Theme.secondary)
                    Spacer()
                    Button(input.changingLocation ? "Done" : "Change location") { input.changingLocation.toggle() }
                        .font(.subheadline)
                }
                if input.changingLocation {
                    InputBox(placeholder: "~/Projects/project", text: $input.into, mono: true)
                } else {
                    Text(input.into.isEmpty ? "Enter a repository to choose its folder" : input.into)
                        .font(Theme.monoSmall).foregroundStyle(Theme.secondary)
                        .textSelection(.enabled)
                }
            }
        } else {
            Field(label: "Repository", hint: "Browse or enter a repository path on the selected machine.") {
                VStack(spacing: 8) {
                    InputBox(placeholder: "~/Projects/project", text: $input.repo, mono: true)
                    FolderBrowser(fleet: fleet, hostId: hostId, picked: $input.repo)
                        .id(hostId)
                        .frame(height: 220)
                }
            }
        }
    }

    private func pick(_ projectId: String, _ candidates: [Machine]) {
        guard let hostId = Draft.machine(for: projectId, among: candidates.map(\.hostId), fleet: fleet) else { return }
        picked(Draft(hostId: hostId, projectId: projectId))
        dismiss()
    }

    private var pathReady: Bool {
        let path = (input.cloning ? input.into : input.repo).trimmingCharacters(in: .whitespaces)
        let source = input.cloning ? !input.url.trimmingCharacters(in: .whitespaces).isEmpty : true
        return machines.contains { $0.hostId == hostId } && source && (path.hasPrefix("/") || path.hasPrefix("~/"))
    }

    /// The folder a clone of `url` goes into by default: one named after the repository, in the
    /// machine's projects folder `dir`.
    nonisolated static func cloneFolder(_ url: String, in dir: String = "~/Projects") -> String {
        var name = url.trimmingCharacters(in: .whitespaces)
        while name.hasSuffix("/") { name.removeLast() }
        name = String(name.split(whereSeparator: { $0 == "/" || $0 == ":" }).last ?? "")
        if name.hasSuffix(".git") { name.removeLast(4) }
        var dir = dir
        while dir.hasSuffix("/") { dir.removeLast() }
        return name.isEmpty ? "" : "\(dir)/\(name)"
    }

    /// Registers the repository as a project on the machine, cloning it there first when asked,
    /// then opens a chat in it.
    private func submitPath() async {
        guard pathReady, !submitting else { return }
        submitting = true
        input.addError = nil
        defer { submitting = false }
        let path = (input.cloning ? input.into : input.repo).trimmingCharacters(in: .whitespaces)
        let cloneURL = input.url.trimmingCharacters(in: .whitespaces)
        do {
            let projectId = input.cloning
                ? try await fleet.cloneProject(cloneURL, into: path, on: hostId)
                : try await fleet.addProject(path, on: hostId)
            picked(Draft(hostId: hostId, projectId: projectId, repo: path))
            dismiss()
        } catch {
            input.addError = describe(error)
        }
    }
}

/// Repository input belongs to one machine. Keep its transitions testable without a daemon.
struct ProjectRepositoryInput {
    var repo = ""
    var cloning = false
    var url = ""
    var into = ""
    /// The machine's projects folder, where a clone goes unless moved.
    var projectsDir = "~/Projects"
    var changingLocation = false
    var addError: String?

    mutating func changeMachine() {
        repo = ""
        projectsDir = "~/Projects"
        into = ProjectPicker.cloneFolder(url, in: projectsDir)
        changingLocation = false
        addError = nil
    }

    mutating func updateCloneDestination(from old: String, to new: String) {
        if into.isEmpty || into == ProjectPicker.cloneFolder(old, in: projectsDir) {
            into = ProjectPicker.cloneFolder(new, in: projectsDir)
        }
    }

    mutating func useExistingRepository() {
        repo = into
        cloning = false
        changingLocation = false
        addError = nil
    }

    /// Moves a clone left in the projects folder to the machine's own, `dir`.
    mutating func setProjectsDir(_ dir: String) {
        if into.isEmpty || into == ProjectPicker.cloneFolder(url, in: projectsDir) {
            into = ProjectPicker.cloneFolder(url, in: dir)
        }
        projectsDir = dir
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

/// An address of a machine, numbered in its order: "In use", a Connect under way or the
/// button that starts one, the buttons that move and remove it, and why a Connect failed.
struct AddressRow: View {
    let index: Int
    let address: String
    let count: Int
    let status: AddressStatus
    /// A Connect is under way, through any address.
    let busy: Bool
    let connect: () -> Void
    let move: (_ to: Int) -> Void
    let remove: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 10) {
                Text("\(index + 1)").monospacedDigit().foregroundStyle(Theme.tertiary)
                Text(address).font(Theme.monoSmall).foregroundStyle(Theme.text).textSelection(.enabled)
                statusView
                Spacer()
                Button("Move Up", systemImage: "chevron.up") { move(index - 1) }
                    .disabled(index == 0)
                Button("Move Down", systemImage: "chevron.down") { move(index + 1) }
                    .disabled(index == count - 1)
                Button("Remove", systemImage: "minus.circle", action: remove)
                    .disabled(count == 1)
            }
            .labelStyle(.iconOnly)
            .buttonStyle(.borderless)
            .font(.subheadline)
            if case .failed(let message) = status {
                Text(message).font(.caption).foregroundStyle(Theme.failure)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.leading, 17)
            }
        }
    }

    @ViewBuilder
    private var statusView: some View {
        switch status {
        case .inUse:
            Text("In use").font(.caption.weight(.semibold)).foregroundStyle(Theme.success)
        case .connecting:
            HStack(spacing: 6) {
                ProgressView().controlSize(.mini)
                Text("Connecting…").font(.caption).foregroundStyle(Theme.tertiary)
            }
        case .available, .failed:
            Button(action: connect) {
                Text("Connect")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(Theme.secondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 2)
                    .background(Theme.raised, in: .capsule)
                    .overlay(Capsule().strokeBorder(Theme.stroke))
            }
            .buttonStyle(.plain)
            .help("Move to the top and reconnect through this address now")
            .disabled(busy)
        }
    }
}

/// What an address in a machine's settings shows beside it.
enum AddressStatus: Equatable {
    /// The connection uses it.
    case inUse
    /// A Connect through it is under way.
    case connecting
    /// The last Connect through it failed, with why; it can be tried again.
    case failed(String)
    /// Not in use; Connect switches the connection to it.
    case available

    /// An address a Connect failed on, and why.
    struct Failure: Equatable {
        let address: String
        let message: String
    }

    /// `address`'s status, given the address the connection uses, the one a Connect is
    /// switching to, and the last Connect's failure.
    init(address: String, inUse: String?, connecting: String?, failure: Failure?) {
        if address == connecting {
            self = .connecting
        } else if address == inUse {
            self = .inUse
        } else if let failure, failure.address == address {
            self = .failed(failure.message)
        } else {
            self = .available
        }
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
        .task(id: copied) {
            guard copied else { return }
            try? await Task.sleep(for: .seconds(1.5))
            copied = false
        }
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
