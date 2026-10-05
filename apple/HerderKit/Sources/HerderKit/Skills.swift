import Foundation
import Herder

/// The skill library as the machines have it: the repository, each machine's checkout of it,
/// and every skill with the machines that have it and whether each has it enabled.
struct SkillLibrary: Equatable {
    /// One machine's checkout of the library.
    struct Checkout: Equatable, Identifiable {
        var id: HostId { hostId }
        let hostId: HostId
        let name: String
        let connected: Bool
        let owner: Bool
        /// Whether the machine has reported its library yet.
        let reported: Bool
        /// The repository the machine's checkout is of; `nil` until one is set.
        let repo: String?
        /// The commit it is at.
        let head: String?
        let lastPull: Date?
        let pullError: String?

        /// Whether the user may change the library through this machine now.
        var writable: Bool { connected && owner && repo != nil }
    }

    /// A skill on one machine.
    struct Presence: Equatable, Identifiable {
        var id: HostId { hostId }
        let hostId: HostId
        let machine: String
        let enabled: Bool
        /// The providers whose CLIs load it there; none while it is disabled.
        let providers: [Provider]
        /// Whether the user may enable or disable it there now.
        let changeable: Bool
    }

    /// A skill of the library.
    struct Skill: Equatable, Identifiable {
        var id: String { name }
        let name: String
        let description: String
        /// The machines that have it, in the machines' order.
        var machines: [Presence]

        /// Every provider it reaches on any machine, by name.
        var providers: [Provider] { Array(Set(machines.flatMap(\.providers))).sorted() }
    }

    /// Every machine but a vault, which keeps no library.
    let checkouts: [Checkout]
    /// Every skill on any machine, by name.
    let skills: [Skill]

    init(_ machines: [Machine]) {
        let daemons = machines.filter(\.hosts.isEmpty)
        checkouts = daemons.map { machine in
            let status = machine.skills
            return Checkout(hostId: machine.hostId, name: machine.name, connected: machine.connection == .connected,
                            owner: machine.role == .owner, reported: status != nil, repo: status?.repo,
                            head: status?.head, lastPull: status?.lastPull.flatMap(Timestamp.date),
                            pullError: status?.pullError)
        }
        var skills: [String: Skill] = [:]
        for (machine, checkout) in zip(daemons, checkouts) {
            for skill in machine.skills?.skills ?? [] {
                let presence = Presence(hostId: machine.hostId, machine: machine.name, enabled: skill.enabled,
                                        providers: skill.providers, changeable: checkout.writable)
                skills[skill.name, default: Skill(name: skill.name, description: skill.description, machines: [])]
                    .machines.append(presence)
            }
        }
        self.skills = skills.values.sorted { $0.name < $1.name }
    }

    /// The library's repository: the first a machine reports, as the client keeps every
    /// machine on it.
    var repo: String? { checkouts.lazy.compactMap(\.repo).first }

    /// The machine a write goes through: the first the user owns that has the library and is
    /// connected. The client then has the others pull it.
    var writer: HostId? { checkouts.first(where: \.writable)?.hostId }

    /// The machine to set the library's repository on: the first connected one the user owns.
    /// The client sets it on every other machine they own.
    var setter: HostId? { checkouts.first { $0.connected && $0.owner }?.hostId }

    /// The machines to pull on: every connected one with a library the user owns.
    var pullable: [HostId] { checkouts.filter(\.writable).map(\.hostId) }

    /// A commit, as short as git shows it.
    static func shortHead(_ head: String) -> String { String(head.prefix(7)) }
}

/// A session's project skills, the ones checked in to its repository, with the project.
struct ProjectSkills: Equatable, Identifiable {
    var id: String { projectId }
    /// Empty for sessions whose project is not known yet.
    let projectId: ProjectId
    let name: String
    /// By path, each once however many sessions see it.
    let skills: [SessionSkill]

    /// The project skills of every project with a session that sees some, by project name.
    static func all(_ machines: [Machine]) -> [ProjectSkills] {
        var found: [ProjectId: (name: String, skills: [String: SessionSkill])] = [:]
        for machine in machines {
            for head in machine.sessions {
                let skills = (machine.sessionSkills[head.sessionId] ?? []).filter { $0.source == .project }
                guard !skills.isEmpty else { continue }
                let projectId = head.projectId ?? ""
                let name = machine.projects.first { $0.projectId == projectId }?.name
                    ?? (projectId.isEmpty ? "No project yet" : projectId)
                found[projectId, default: (name: name, skills: [:])].skills.merge(
                    skills.map { ($0.path ?? $0.name, $0) }, uniquingKeysWith: { first, _ in first })
            }
        }
        return found.map { projectId, entry in
            ProjectSkills(projectId: projectId, name: entry.name,
                          skills: entry.skills.sorted { $0.key < $1.key }.map(\.value))
        }
        .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }
}

/// A skill as the app writes it: one `SKILL.md`, its front matter naming and describing it.
struct SkillDocument: Equatable {
    var name = ""
    var description = ""
    var body = ""

    /// Most characters a skill name may have, as the protocol allows.
    static let maxNameCharacters = 64

    /// Whether `name` may name a skill, as the daemon checks it: lowercase ASCII letters,
    /// digits and hyphens, neither starting nor ending with a hyphen nor holding two in a row.
    static func isValidName(_ name: String) -> Bool {
        !name.isEmpty && name.count <= maxNameCharacters
            && name.allSatisfy { $0.isASCII && ($0.isLowercase || $0.isNumber || $0 == "-") }
            && !name.hasPrefix("-") && !name.hasSuffix("-") && !name.contains("--")
    }

    /// The `SKILL.md`: front matter with the name and the description on one line, then the body.
    var markdown: String {
        let description = description.split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }.joined(separator: " ")
        let body = body.trimmingCharacters(in: .whitespacesAndNewlines)
        return "---\nname: \(name)\ndescription: \(Self.yamlScalar(description))\n---\n\n\(body)\n"
    }

    var files: [SkillFile] {
        [SkillFile(path: "SKILL.md", data: Data(markdown.utf8), executable: false)]
    }

    /// `text` as a YAML scalar on one line: bare when YAML reads it back unchanged, quoted
    /// otherwise.
    static func yamlScalar(_ text: String) -> String {
        let special = ": ", comment = " #"
        let plain = !text.isEmpty && !text.contains(special) && !text.contains(comment) && !text.hasSuffix(":")
            && !"-?:,[]{}#&*!|>'\"%@`".contains(text.first ?? " ")
        if plain { return text }
        if !text.contains("\"") && !text.contains("\\") { return "\"\(text)\"" }
        return "'\(text.replacingOccurrences(of: "'", with: "''"))'"
    }
}

extension Fleet {
    /// Makes the git repository at `url` the skill library, through `hostId`; the client sets
    /// it on every other machine the user owns.
    func setSkillsRepo(_ url: String, on hostId: HostId) async throws {
        _ = try await client.send(hostId: hostId, command: .setSkillsRepo(url: url))
    }

    /// Adds `document` to the library, or replaces the skill of its name with it.
    func putSkill(_ document: SkillDocument, on hostId: HostId) async throws {
        _ = try await client.send(hostId: hostId, command: .putSkill(name: document.name, files: document.files))
    }

    func deleteSkill(_ name: String, on hostId: HostId) async throws {
        _ = try await client.send(hostId: hostId, command: .deleteSkill(name: name))
    }

    /// Copies the skill folder `path` of the repository at `gitURL`, or the repository's top
    /// when `path` is empty, into the library.
    func importSkill(gitURL: String, path: String, on hostId: HostId) async throws {
        _ = try await client.send(hostId: hostId, command: .importSkill(gitUrl: gitURL, path: path.isEmpty ? nil : path))
    }

    /// Enables or disables a skill on one machine only.
    func setSkillEnabled(_ name: String, _ enabled: Bool, on hostId: HostId) async throws {
        _ = try await client.send(hostId: hostId, command: .setSkillEnabled(name: name, enabled: enabled))
    }

    /// Pulls the library on every connected machine the user owns that has one, one after
    /// another; what failed, with why.
    func pullSkills() async -> [String] {
        var failures: [String] = []
        for hostId in SkillLibrary(machines).pullable {
            do {
                _ = try await client.send(hostId: hostId, command: .pullSkills)
            } catch {
                let name = machines.first { $0.hostId == hostId }?.name ?? hostId
                failures.append("\(name): \(describe(error))")
            }
        }
        return failures
    }
}
