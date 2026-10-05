import Herder
import SwiftUI
import UniformTypeIdentifiers

/// Skills: the skill library, its checkout on each machine, every skill with the providers it
/// reaches and a switch per machine, the skills in each account's own folder, and the project
/// skills of a session's repository. Owners add, import, edit and delete skills, and sync the
/// library through a git repository; members only look.
struct SkillsView: View {
    let fleet: Fleet
    /// The session open beside the screen, whose project's skills it lists first.
    var session: SessionKey?
    @State private var editing: SkillEditing?
    @State private var importing = false
    @State private var deleting: SkillLibrary.Skill?
    @State private var repoURL = ""
    @State private var projectId: ProjectId?
    @State private var failures: [String] = []

    var body: some View {
        let library = SkillLibrary(fleet.machines)
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                if library.checkouts.isEmpty {
                    Text("No machine yet.").font(.footnote).foregroundStyle(Theme.tertiary)
                } else {
                    header(library)
                    skills(library)
                    if library.repo == nil {
                        setup(library)
                    } else {
                        checkouts(library)
                    }
                }
                accountSkills
                projectSkills
                ForEach(failures, id: \.self) { failure in
                    Text(failure).font(.footnote).foregroundStyle(Theme.failure)
                }
            }
            .frame(maxWidth: 760, alignment: .leading)
            .padding(.horizontal, 16)
            .padding(.vertical, 16)
            .frame(maxWidth: .infinity)
        }
        .background(Theme.background)
        .navigationTitle("Skills")
        .refreshable { failures = await fleet.pullSkills() }
        .sheet(item: $editing) { editing in
            SkillEditorSheet(fleet: fleet, editing: editing)
        }
        .sheet(isPresented: $importing) { SkillImportSheet(fleet: fleet) }
        .confirmationDialog("Delete \(deleting?.name ?? "")?", isPresented: Binding(
            get: { deleting != nil }, set: { if !$0 { deleting = nil } }), titleVisibility: .visible) {
            Button("Delete", role: .destructive) {
                if let deleting { delete(deleting.name) }
            }
        } message: {
            Text("It is removed from the library on every machine.")
        }
    }

    /// While each machine keeps its own library: syncing it through a git repository.
    @ViewBuilder private func setup(_ library: SkillLibrary) -> some View {
        Card {
            VStack(alignment: .leading, spacing: 12) {
                Text("Sync with a git repository").font(.headline).foregroundStyle(Theme.text)
                Text("One folder per skill, each with a SKILL.md. Every machine keeps a checkout, pulls what another pushed, and hands the skills to the provider CLIs. The skills each machine has now are pushed to it.")
                    .font(.subheadline).foregroundStyle(Theme.secondary)
                if let setter = library.setter {
                    HStack(spacing: 10) {
                        InputBox(placeholder: "git@github.com:you/skills.git", text: $repoURL, mono: true)
                            .accessibilityIdentifier("skills-repo-url")
                        ActionButton(title: "Use", style: .primary) {
                            await run { try await fleet.setSkillsRepo(repoURL.trimmingCharacters(in: .whitespaces), on: setter) }
                        }
                        .frame(width: 100)
                        .disabled(repoURL.trimmingCharacters(in: .whitespaces).isEmpty)
                    }
                    Text("Each machine clones it with its own git access, as it pushes checkpoints.")
                        .font(.footnote).foregroundStyle(Theme.tertiary)
                } else {
                    Label("Only a machine's owners can sync its library.", systemImage: "lock")
                        .font(.footnote).foregroundStyle(Theme.tertiary)
                }
            }
        }
    }

    /// The repository, and the buttons that change the library.
    private func header(_ library: SkillLibrary) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            if let repo = library.repo {
                Text(repo).font(Theme.mono).foregroundStyle(Theme.text).lineLimit(1).truncationMode(.middle)
                    .textSelection(.enabled)
            } else {
                Text("Kept on each of your machines; not synced.").font(.subheadline).foregroundStyle(Theme.secondary)
            }
            FlowLayout(spacing: 8) {
                if library.repo != nil {
                    control("Pull", symbol: "arrow.down.circle") { failures = await fleet.pullSkills() }
                        .disabled(library.pullable.isEmpty)
                }
                if library.writer != nil {
                    control("New Skill", symbol: "plus") { editing = .new }
                    control("Import", symbol: "square.and.arrow.down") { importing = true }
                }
            }
            if library.writer == nil {
                Label(library.checkouts.contains(where: \.owner) ? "Connect a machine you own to change the library."
                                                                 : "Only a machine's owners can change its library.",
                      systemImage: "lock")
                    .font(.footnote).foregroundStyle(Theme.tertiary)
            }
        }
    }

    private func control(_ title: String, symbol: String, action: @escaping () async -> Void) -> some View {
        Button { Task { await action() } } label: {
            Chip(symbol: symbol, text: title, tint: Theme.text)
                .frame(minHeight: 30)
                .hitTarget()
        }
        .buttonStyle(.plain)
    }

    /// Each machine's checkout: its commit, when it last pulled, and why a pull failed.
    private func checkouts(_ library: SkillLibrary) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionHeading(title: "Machines", count: library.checkouts.count)
            Card(padding: 0) {
                VStack(spacing: 0) {
                    ForEach(Array(library.checkouts.enumerated()), id: \.element.id) { index, checkout in
                        if index > 0 { Rectangle().fill(Theme.stroke).frame(height: 1) }
                        CheckoutRow(checkout: checkout, repo: library.repo)
                    }
                }
            }
        }
    }

    @ViewBuilder private func skills(_ library: SkillLibrary) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionHeading(title: "Library", count: library.skills.count)
            if library.skills.isEmpty {
                Text("No skills yet.").font(.footnote).foregroundStyle(Theme.tertiary)
            }
            ForEach(library.skills) { skill in
                LibrarySkillRow(skill: skill, editable: library.writer != nil,
                                edit: { editing = .existing(skill.name, description: skill.description) },
                                delete: { deleting = skill },
                                setEnabled: { hostId, enabled in
                                    Task { await run { try await fleet.setSkillEnabled(skill.name, enabled, on: hostId) } }
                                })
            }
        }
    }

    /// The skills in each account's own folder, which its CLI loads; read-only here.
    @ViewBuilder private var accountSkills: some View {
        let lists = AccountSkillList.all(fleet.machines)
        if !lists.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                SectionHeading(title: "Account Skills", count: lists.map(\.skills.count).reduce(0, +))
                ForEach(lists) { list in
                    Text(list.title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    ForEach(list.skills, id: \.self) { SourcedSkillRow(skill: $0) }
                }
                Text("In each account's own skills folder, such as ~/.claude/skills, with the skills Claude syncs from claude.ai; change them there.")
                    .font(.footnote).foregroundStyle(Theme.tertiary)
            }
        }
    }

    /// The project skills of one project: the open session's, or one picked.
    @ViewBuilder private var projectSkills: some View {
        let projects = ProjectSkills.all(fleet.machines)
        let open = session.flatMap { key in
            fleet.machines.first { $0.hostId == key.hostId }?.sessions.first { $0.sessionId == key.sessionId }?.projectId
        }
        let shown = projects.first { $0.projectId == projectId } ?? projects.first { $0.projectId == open }
            ?? projects.first
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                SectionHeading(title: "Project Skills", count: shown?.skills.count)
                Spacer()
                if projects.count > 1, let shown {
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
                }
            }
            if let shown {
                if projects.count == 1 {
                    Text(shown.name).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                }
                ForEach(shown.skills, id: \.self) { SourcedSkillRow(skill: $0) }
                Text("Checked in to the repository; change them there.")
                    .font(.footnote).foregroundStyle(Theme.tertiary)
            } else {
                Text("Skills a project checks in, in .claude/skills or .agents/skills, show here once one of its sessions runs.")
                    .font(.footnote).foregroundStyle(Theme.tertiary)
            }
        }
    }

    private func delete(_ name: String) {
        Task { await run { try await fleet.deleteSkill(name) } }
    }

    private func run(_ action: () async throws -> Void) async {
        do {
            try await action()
            failures = []
        } catch {
            failures = [describe(error)]
        }
    }
}

/// One machine's checkout of the library.
private struct CheckoutRow: View {
    let checkout: SkillLibrary.Checkout
    let repo: String?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: "server.rack").foregroundStyle(Theme.secondary)
            VStack(alignment: .leading, spacing: 3) {
                Text(checkout.name).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                Text(state).font(.caption).foregroundStyle(Theme.secondary).lineLimit(2)
                if let error = checkout.pullError {
                    Text(error).font(.caption).foregroundStyle(Theme.failure).lineLimit(3)
                }
            }
            Spacer()
            if let head = checkout.head {
                Text(SkillLibrary.shortHead(head)).font(Theme.monoSmall).foregroundStyle(Theme.text)
                    .textSelection(.enabled)
            }
        }
        .padding(12)
        .accessibilityElement(children: .combine)
    }

    private var state: String {
        if !checkout.connected { return "Not connected" }
        if !checkout.reported { return "Has not reported its library yet" }
        guard let own = checkout.repo else { return "No library yet" }
        if own != repo { return "On another repository: \(own)" }
        guard let pulled = checkout.lastPull else { return "Not pulled yet" }
        let age = Timestamp.age(pulled, now: .now)
        return age == "now" ? "Pulled just now" : "Pulled \(age) ago"
    }
}

/// A library skill: its name and description, the providers it reaches, and a switch per
/// machine.
private struct LibrarySkillRow: View {
    let skill: SkillLibrary.Skill
    let editable: Bool
    let edit: () -> Void
    let delete: () -> Void
    let setEnabled: (HostId, Bool) -> Void

    var body: some View {
        Card(padding: 12) {
            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    VStack(alignment: .leading, spacing: 3) {
                        Text(skill.name).font(Theme.mono.weight(.semibold)).foregroundStyle(Theme.text)
                        if !skill.description.isEmpty {
                            Text(skill.description).font(.subheadline).foregroundStyle(Theme.secondary)
                        }
                    }
                    Spacer()
                    if editable {
                        Menu {
                            Button("Edit", systemImage: "pencil", action: edit)
                            Button("Delete", systemImage: "trash", role: .destructive, action: delete)
                        } label: {
                            Image(systemName: "ellipsis").foregroundStyle(Theme.secondary)
                                .frame(width: 30, height: 30).hitTarget()
                        }
                        .menuStyle(.button)
                        .buttonStyle(.plain)
                        .fixedSize()
                        .accessibilityLabel("\(skill.name) actions")
                    }
                }
                FlowLayout(spacing: 6) {
                    if skill.providers.isEmpty {
                        Chip(text: "Reaches no CLI")
                    }
                    ForEach(skill.providers, id: \.self) { provider in
                        HStack(spacing: 4) {
                            ProviderMark(provider: provider, size: 11)
                            Text(ModelCatalog.providerName(provider))
                        }
                        .font(.caption.weight(.medium))
                        .foregroundStyle(Theme.secondary)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(Theme.raised, in: .capsule)
                    }
                }
                ForEach(skill.machines) { presence in
                    Toggle(isOn: Binding(get: { presence.enabled }, set: { setEnabled(presence.hostId, $0) })) {
                        Text("On \(presence.machine)").font(.subheadline).foregroundStyle(Theme.text)
                    }
                    .toggleStyle(.switch)
                    .disabled(!presence.changeable)
                    .accessibilityIdentifier("skill-\(skill.name)-\(presence.hostId)")
                }
            }
        }
    }
}

/// A project or account skill, read-only, labelled with where it is.
private struct SourcedSkillRow: View {
    let skill: SessionSkill

    var body: some View {
        Card(padding: 12) {
            VStack(alignment: .leading, spacing: 6) {
                Text(skill.name).font(Theme.mono.weight(.semibold)).foregroundStyle(Theme.text)
                if !skill.description.isEmpty {
                    Text(skill.description).font(.subheadline).foregroundStyle(Theme.secondary)
                }
                Chip(symbol: "folder", text: "\(skill.sourceLabel) · \(skill.path ?? skill.name)")
            }
        }
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
                      subtitle: "Saved to the library on your machines") {
            Field(label: "Name", hint: "Lowercase letters, digits and hyphens; the skill's folder. Agents use it as `$name`.") {
                InputBox(placeholder: "review-pr", text: $document.name, mono: true)
                    .disabled(!isNew)
                    .accessibilityIdentifier("skill-name")
            }
            Field(label: "Description", hint: "What the skill is for and when to use it; agents read this to pick it.") {
                InputBox(placeholder: "Review a pull request for correctness and tests.", text: $document.description,
                         lines: 1...3)
                    .accessibilityIdentifier("skill-description")
            }
            Field(label: "Instructions",
                  hint: isNew ? "The body of SKILL.md, in Markdown."
                              : "The body of SKILL.md, in Markdown. Saving replaces the skill's folder with this SKILL.md, so any other files in it are removed.") {
                InputBox(placeholder: "Steps the agent follows…", text: $document.body, mono: true, lines: 8...20)
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
        SheetScaffold(title: "Import Skill", subtitle: "Copied into the library on your machines", height: 480) {
            ChoiceChips(options: [(Source.folder, "Folder", "On this device"), (.git, "Git", "From a repository")],
                        selection: $source)
            switch source {
            case .folder:
                Field(label: "Folder", hint: "A skill folder with a SKILL.md at its top; every file in it is copied.") {
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
                        .frame(minHeight: 44)
                        .background(Theme.raised, in: .rect(cornerRadius: Theme.corner))
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("skill-import-folder")
                }
                if folder != nil {
                    Field(label: "Name", hint: "Lowercase letters, digits and hyphens. Agents use it as `$name`.") {
                        InputBox(placeholder: "review-pr", text: $name, mono: true)
                            .accessibilityIdentifier("skill-import-name")
                    }
                }
            case .git:
                Field(label: "Repository", hint: "Cloned by the machine, with its own git access.") {
                    InputBox(placeholder: "https://github.com/you/skills.git", text: $url, mono: true)
                        .accessibilityIdentifier("skill-import-url")
                }
                Field(label: "Folder", hint: "The skill's folder in the repository, which names it; leave it empty when the skill is the whole repository.") {
                    InputBox(placeholder: "skills/review-pr", text: $path, mono: true)
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
