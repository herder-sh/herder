import Herder
import SwiftUI
import UniformTypeIdentifiers

/// The Skills page's tabs.
enum SkillsTab: String, Hashable {
    case library, account, project

    /// What the tab lists, for its ⓘ and its empty state.
    var about: String {
        switch self {
        case .library:
            "One folder per skill, each with a SKILL.md. Every machine keeps the library and hands the skills it has enabled to its provider CLIs; a switch turns a skill on or off on one machine. Synced through a git repository, each machine pulls what another pushed, and a dot marks a skill a machine that is behind does not have yet."
        case .account:
            "The skills in each account's own folder, such as ~/.claude/skills, with the skills Claude syncs from claude.ai. The account's CLI loads them; change them there."
        case .project:
            "The skills a project checks in, in .claude/skills or .agents/skills. They show once one of its sessions runs; change them in the repository."
        }
    }
}

/// Skills, in three tabs. **Library**: every skill with the providers it reaches and a switch
/// per machine, then one line on how the machines' checkouts of the library stand. **Account**:
/// the skills in each account's own folder. **Project**: the skills checked in to a project.
/// Owners add, import, edit and delete skills (`SkillActions`) and sync the library through a git
/// repository; members only look.
struct SkillsView: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    /// The session open beside the screen, whose project's skills the Project tab shows first.
    var session: SessionKey?

    var body: some View {
        ScrollView {
            SkillsPage(fleet: fleet, sheet: $sheet, session: session)
                .pageColumn()
        }
        .background(.ground)
        .navigationTitle("Skills")
        .toolbar {
            #if os(iOS)
            SkillActions(fleet: fleet, sheet: $sheet)
            #endif
        }
        .refreshable { await SkillActions.pull(fleet) }
    }
}

/// What `SkillsView` scrolls: the tabs and the one shown.
struct SkillsPage: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?
    var session: SessionKey?
    @State private var tab = SkillsTab.library
    @State private var deleting: SkillLibrary.Skill?
    @State private var repoURL = ""
    @State private var projectId: ProjectId?

    var body: some View {
        let library = SkillLibrary(fleet.machines)
        let accounts = AccountSkillList.all(fleet.machines)
        let projects = ProjectSkills.all(fleet.machines)
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 6) {
                tabButton(.library, "Library", count: library.skills.count)
                tabButton(.account, "Account", count: accounts.map(\.skills.count).reduce(0, +))
                tabButton(.project, "Project", count: projects.map(\.skills.count).reduce(0, +))
                Spacer()
                InfoButton(text: tab.about)
            }
            switch tab {
            case .library: libraryTab(library)
            case .account: accountTab(accounts)
            case .project: projectTab(projects)
            }
        }
        .confirmationDialog("Delete \(deleting?.name ?? "")?", isPresented: Binding(
            get: { deleting != nil }, set: { if !$0 { deleting = nil } }), titleVisibility: .visible) {
            Button("Delete", role: .destructive) {
                if let deleting { run("Could not delete \(deleting.name)") { try await fleet.deleteSkill(deleting.name) } }
            }
        } message: {
            Text("It is removed from the library on every machine.")
        }
    }

    private func tabButton(_ value: SkillsTab, _ title: String, count: Int) -> some View {
        let selected = tab == value
        return Button { tab = value } label: {
            HStack(spacing: 5) {
                Text(title).foregroundStyle(selected ? Theme.text : Theme.secondary)
                Text("\(count)").monospacedDigit().foregroundStyle(Theme.tertiary)
            }
            .font(Theme.body.weight(.medium))
            .padding(.horizontal, 10)
            .frame(height: 28)
            .background(selected ? Theme.raised : .clear, in: .rect(cornerRadius: 8))
            .hitTarget()
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("skills-tab-\(value.rawValue)")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    // MARK: Library

    @ViewBuilder private func libraryTab(_ library: SkillLibrary) -> some View {
        if library.checkouts.isEmpty {
            EmptySkills(title: "No machine yet", text: "Skills live on your machines; add one under Fleet.")
        } else {
            if library.skills.isEmpty {
                EmptySkills(title: "No skills yet",
                            text: (library.writer == nil ? "" : "Write one with New Skill, or import a folder or a git repository. ")
                                + SkillsTab.library.about)
            } else {
                SortableTable("skills.library", rows: library.skills, sortedBy: TableSort(column: "Skill"),
                              columns: columns(library))
            }
            if library.repo == nil { setup(library) } else { health(library) }
            if library.writer == nil {
                Label(library.checkouts.contains(where: \.owner) ? "Connect a machine you own to change the library."
                                                                 : "Only a machine's owners can change its library.",
                      systemImage: "lock")
                    .font(Theme.meta).foregroundStyle(Theme.tertiary)
            }
        }
    }

    /// The skill and its description, the providers it reaches, a column per machine, and the
    /// skill's menu for an owner.
    private func columns(_ library: SkillLibrary) -> [SortableColumn<SkillLibrary.Skill>] {
        var columns: [SortableColumn<SkillLibrary.Skill>] = [
            SortableColumn("Skill", value: \.name) { SkillLabel(name: $0.name, description: $0.description) },
            SortableColumn("Providers", width: 70, value: { $0.providers.count }) { skill in
                HStack(spacing: 5) {
                    ForEach(skill.providers, id: \.self) { provider in
                        ProviderMark(provider: provider, size: 13).help(ModelCatalog.providerName(provider))
                    }
                    if skill.providers.isEmpty { Text("None").font(Theme.meta).foregroundStyle(Theme.tertiary) }
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(skill.providers.isEmpty ? "Reaches no CLI"
                                    : skill.providers.map(ModelCatalog.providerName).joined(separator: ", "))
            },
        ]
        let behind = Set(library.behind.map(\.hostId))
        for checkout in library.checkouts {
            columns.append(SortableColumn(checkout.name, width: 84, alignment: .center) { skill in
                MachineCell(skill: skill, checkout: checkout, behind: behind.contains(checkout.hostId)) { enabled in
                    run("Could not turn \(skill.name) \(enabled ? "on" : "off")") {
                        try await fleet.setSkillEnabled(skill.name, enabled, on: checkout.hostId)
                    }
                }
            })
        }
        if library.writer != nil {
            columns.append(SortableColumn("", width: 30, alignment: .trailing) { skill in
                Menu {
                    Button("Edit", systemImage: "pencil") { sheet = .skill(.existing(skill.name, description: skill.description)) }
                    Button("Delete", systemImage: "trash", role: .destructive) { deleting = skill }
                } label: {
                    Image(systemName: "ellipsis").foregroundStyle(Theme.secondary).frame(width: 30, height: 24).hitTarget()
                }
                .menuStyle(.button)
                .buttonStyle(.plain)
                .menuIndicator(.hidden)
                .fixedSize()
                .accessibilityLabel("\(skill.name) actions")
            })
        }
        return columns
    }

    /// The library's sync health: its repository, then who is behind or stuck with a pull,
    /// else the commit every machine is at.
    private func health(_ library: SkillLibrary) -> some View {
        let off = !library.behind.isEmpty || !library.stuck.isEmpty
        return VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(library.repo ?? "").font(Theme.monoSmall).lineLimit(1).truncationMode(.middle)
                    .textSelection(.enabled)
                Text("·")
                Text(library.health).foregroundStyle(off ? Theme.text : Theme.secondary).lineLimit(1)
                if off && !library.pullable.isEmpty {
                    Button("Pull now") { Task { await SkillActions.pull(fleet) } }
                        .buttonStyle(.plain)
                        .underline()
                        .foregroundStyle(Theme.text)
                        .fixedSize()
                }
            }
            .font(Theme.meta)
            .foregroundStyle(Theme.secondary)
            ForEach(library.stuck.filter { $0.pullError != nil }) { checkout in
                Text("\(checkout.name): \(checkout.pullError ?? "")")
                    .font(Theme.meta).foregroundStyle(Theme.failure).lineLimit(2).textSelection(.enabled)
            }
        }
        .padding(.top, 4)
    }

    /// While each machine keeps its own library: syncing it through a git repository.
    private func setup(_ library: SkillLibrary) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 4) {
                Text("Kept on each machine; not synced.").font(Theme.meta).foregroundStyle(Theme.secondary)
                InfoButton(text: "Sync the library through a git repository: every machine keeps a checkout, pulls what another pushed, and pushes the skills it has now. Each clones it with its own git access, as it pushes checkpoints.")
            }
            if let setter = library.setter {
                HStack(spacing: 8) {
                    InputBox(placeholder: "git@github.com:you/skills.git", text: $repoURL, mono: true)
                        .accessibilityIdentifier("skills-repo-url")
                    ActionButton(title: "Sync", style: .primary) {
                        let url = repoURL.trimmingCharacters(in: .whitespaces)
                        do { try await fleet.setSkillsRepo(url, on: setter) } catch {
                            fleet.toast = .failure("Could not sync with \(url)", error)
                        }
                    }
                    .frame(width: 100)
                    .disabled(repoURL.trimmingCharacters(in: .whitespaces).isEmpty)
                }
                .frame(maxWidth: 560)
            }
        }
        .padding(.top, 4)
    }

    // MARK: Account and project

    @ViewBuilder private func accountTab(_ lists: [AccountSkillList]) -> some View {
        if lists.isEmpty {
            EmptySkills(title: "No account skills", text: SkillsTab.account.about)
        } else {
            let rows = lists.flatMap { list in list.skills.map { SourcedSkill(id: "\(list.id)/\($0.path ?? $0.name)", skill: $0, place: list.title) } }
            SortableTable("skills.account", rows: rows, sortedBy: TableSort(column: "Skill"), columns: [
                SortableColumn("Skill", value: \.skill.name) { SkillLabel(name: $0.skill.name, description: $0.skill.description) },
                SortableColumn("Account", width: 220, value: \.place) {
                    Text($0.place).font(Theme.meta).foregroundStyle(Theme.secondary).lineLimit(1)
                },
            ])
        }
    }

    /// The project skills of one project: the open session's, or one picked.
    @ViewBuilder private func projectTab(_ projects: [ProjectSkills]) -> some View {
        let open = session.flatMap { key in
            fleet.machines.first { $0.hostId == key.hostId }?.sessions.first { $0.sessionId == key.sessionId }?.projectId
        }
        if let shown = projects.first(where: { $0.projectId == projectId }) ?? projects.first(where: { $0.projectId == open })
            ?? projects.first {
            Menu {
                ForEach(projects) { project in
                    Button(project.name) { projectId = project.projectId }
                }
            } label: {
                Chip(symbol: "folder", text: shown.name).frame(minHeight: 30).hitTarget()
            }
            .menuStyle(.button)
            .buttonStyle(.plain)
            .fixedSize()
            .disabled(projects.count == 1)
            let rows = shown.skills.map { SourcedSkill(id: $0.path ?? $0.name, skill: $0, place: $0.path ?? $0.name) }
            SortableTable("skills.project", rows: rows, sortedBy: TableSort(column: "Skill"), columns: [
                SortableColumn("Skill", value: \.skill.name) { SkillLabel(name: $0.skill.name, description: $0.skill.description) },
                SortableColumn("Folder", width: 220, value: \.place) {
                    Text($0.place).font(Theme.monoSmall).foregroundStyle(Theme.secondary).lineLimit(1).truncationMode(.middle)
                },
            ])
        } else {
            EmptySkills(title: "No project skills", text: SkillsTab.project.about)
        }
    }

    /// Runs a change of the library; a refusal shows as a toast headed `failure`.
    private func run(_ failure: String, _ action: @escaping () async throws -> Void) {
        Task {
            do { try await action() } catch { fleet.toast = .failure(failure, error) }
        }
    }
}

/// The Skills page's actions: **Pull** once there is a repository, **Import** and **New Skill**
/// for an owner.
struct SkillActions: View {
    let fleet: Fleet
    @Binding var sheet: AppSheet?

    var body: some View {
        let library = SkillLibrary(fleet.machines)
        if library.repo != nil {
            button("Pull", symbol: "arrow.down.circle") { Task { await Self.pull(fleet) } }
                .disabled(library.pullable.isEmpty)
        }
        if library.writer != nil {
            button("Import", symbol: "square.and.arrow.down") { sheet = .importSkill }
            button("New Skill", symbol: "plus", prominent: true) { sheet = .skill(.new) }
        }
    }

    @ViewBuilder private func button(_ title: String, symbol: String, prominent: Bool = false,
                                     action: @escaping () -> Void) -> some View {
        #if os(iOS)
        Button(title, systemImage: symbol, action: action)
        #else
        PaneButton(title: title, symbol: symbol, prominent: prominent, action: action)
        #endif
    }

    /// Pulls the library on every machine that can; what failed shows as a toast.
    static func pull(_ fleet: Fleet) async {
        let failures = await fleet.pullSkills()
        if !failures.isEmpty {
            fleet.toast = Toast(text: "Could not pull the skills", detail: failures.joined(separator: "\n"), failed: true)
        }
    }
}

/// A skill's name, with its description under it.
private struct SkillLabel: View {
    let name: String
    let description: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(name).font(Theme.mono.weight(.semibold)).foregroundStyle(Theme.text)
            if !description.isEmpty {
                Text(description).font(Theme.meta).foregroundStyle(Theme.secondary).lineLimit(2)
            }
        }
    }
}

/// A skill's cell in a machine's column: its switch there; an amber dot when the machine is
/// behind and does not have it yet; a dash when the machine does not have it.
private struct MachineCell: View {
    let skill: SkillLibrary.Skill
    let checkout: SkillLibrary.Checkout
    let behind: Bool
    let setEnabled: (Bool) -> Void

    var body: some View {
        if let presence = skill.machines.first(where: { $0.hostId == checkout.hostId }) {
            Toggle(isOn: Binding(get: { presence.enabled }, set: setEnabled)) {
                Text("\(skill.name) on \(checkout.name)")
            }
            .toggleStyle(.switch)
            .labelsHidden()
            .controlSize(.mini)
            .disabled(!presence.changeable)
            .accessibilityIdentifier("skill-\(skill.name)-\(checkout.hostId)")
        } else if behind {
            Circle().fill(Theme.accent).frame(width: 7, height: 7)
                .hitTarget()
                .help("\(checkout.name) is behind; pull to get \(skill.name)")
                .accessibilityLabel("\(checkout.name) is behind")
        } else {
            Text("–").foregroundStyle(Theme.tertiary)
                .help("Not on \(checkout.name)")
                .accessibilityLabel("Not on \(checkout.name)")
        }
    }
}

/// An account or project skill, with where it is.
private struct SourcedSkill: Identifiable {
    let id: String
    let skill: SessionSkill
    /// The account and machine, or the folder in the repository.
    let place: String
}

/// A tab with nothing to list: what it would list, and where that comes from.
private struct EmptySkills: View {
    let title: String
    let text: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.headline).foregroundStyle(Theme.text)
            Text(text).font(Theme.body).foregroundStyle(Theme.secondary).fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: 560, alignment: .leading)
        .padding(.vertical, 24)
    }
}

/// What the skill editor writes: a new skill, or one of the library's.
enum SkillEditing: Identifiable, Hashable {
    case new
    case existing(String, description: String)

    var id: Self { self }
}

/// Writes a skill's `SKILL.md`: its name, description and instructions.
struct SkillEditorSheet: View {
    let fleet: Fleet
    let editing: SkillEditing
    @State private var document = SkillDocument()
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    private var isNew: Bool { editing == .new }

    var body: some View {
        SheetScaffold(title: isNew ? "New Skill" : "Edit \(document.name)",
                      subtitle: "Saved to the library on your machines",
                      hint: isNew ? "Agents use it as `$name`." : "Saving replaces the skill's folder: its other files are removed.") {
            Field(label: "Name") {
                InputBox(placeholder: "review-pr", text: $document.name, mono: true)
                    .disabled(!isNew)
                    .accessibilityIdentifier("skill-name")
            }
            Field(label: "Description") {
                InputBox(placeholder: "When to use it, such as reviewing a pull request", text: $document.description,
                         lines: 1...3)
                    .accessibilityIdentifier("skill-description")
            }
            Field(label: "Instructions") {
                InputBox(placeholder: "Steps the agent follows, in Markdown…", text: $document.body, mono: true, lines: 8...20)
                    .accessibilityIdentifier("skill-body")
            }
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
            }
        } footer: {
            Spacer()
            ActionButton(title: "Save", style: .primary) { await save() }
                .frame(maxWidth: 200)
                .disabled(!SkillDocument.isValidName(document.name)
                          || document.description.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
        .onAppear {
            if case .existing(let name, let description) = editing {
                document = SkillDocument(name: name, description: description)
            }
        }
    }

    private func save() async {
        do {
            try await fleet.putSkill(document.name, files: document.files)
            dismiss()
        } catch {
            self.error = describe(error)
        }
    }
}

/// Imports a skill: a folder picked on this device, or one in a git repository.
struct SkillImportSheet: View {
    enum Source: Hashable { case folder, git }

    let fleet: Fleet
    @State private var source = Source.folder
    @State private var choosing = false
    @State private var folder: URL?
    @State private var files: [SkillFile] = []
    @State private var name = ""
    @State private var url = ""
    @State private var path = ""
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        SheetScaffold(title: "Import Skill", subtitle: "Copied into the library on your machines", height: 440,
                      hint: source == .folder ? "A folder with a SKILL.md at its top; all of it is copied."
                                              : "The machine clones it with its own git access.") {
            ChoiceChips(options: [(Source.folder, "Folder", "On this device"), (.git, "Git", "From a repository")],
                        selection: $source)
            switch source {
            case .folder:
                Field(label: "Folder") {
                    Button { choosing = true } label: {
                        HStack(spacing: 8) {
                            Image(systemName: "folder")
                            Text(folder.map { "\($0.lastPathComponent) · \(files.count) file\(files.count == 1 ? "" : "s")" }
                                 ?? "Choose a Folder…")
                            Spacer()
                        }
                        .font(.subheadline)
                        .foregroundStyle(Theme.text)
                        .padding(.horizontal, 12)
                        .frame(minHeight: Theme.rowHeight)
                        .background(Theme.raised, in: .rect(cornerRadius: Theme.corner))
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("skill-import-folder")
                }
                if folder != nil {
                    Field(label: "Name") {
                        InputBox(placeholder: "review-pr", text: $name, mono: true)
                            .accessibilityIdentifier("skill-import-name")
                    }
                }
            case .git:
                Field(label: "Repository") {
                    InputBox(placeholder: "https://github.com/you/skills.git", text: $url, mono: true)
                        .accessibilityIdentifier("skill-import-url")
                }
                Field(label: "Folder") {
                    InputBox(placeholder: "skills/review-pr, or empty for the whole repository", text: $path, mono: true)
                        .accessibilityIdentifier("skill-import-path")
                }
            }
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.failure)
            }
        } footer: {
            Spacer()
            ActionButton(title: "Import", style: .primary) { await importSkill() }
                .frame(maxWidth: 200)
                .disabled(!ready)
        }
        .fileImporter(isPresented: $choosing, allowedContentTypes: [.folder]) { result in
            pick(result)
        }
    }

    private var ready: Bool {
        switch source {
        case .folder: !files.isEmpty && SkillDocument.isValidName(name)
        case .git: !url.trimmingCharacters(in: .whitespaces).isEmpty
        }
    }

    private func pick(_ result: Result<URL, Error>) {
        do {
            let picked = try result.get()
            files = try SkillFolder.files(at: picked)
            folder = picked
            name = SkillFolder.name(of: picked.lastPathComponent)
            error = nil
        } catch {
            folder = nil
            files = []
            self.error = describe(error)
        }
    }

    private func importSkill() async {
        do {
            switch source {
            case .folder:
                try await fleet.putSkill(name, files: files)
            case .git:
                try await fleet.importSkill(gitURL: url.trimmingCharacters(in: .whitespaces),
                                            path: path.trimmingCharacters(in: .whitespaces))
            }
            dismiss()
        } catch {
            self.error = describe(error)
        }
    }
}
