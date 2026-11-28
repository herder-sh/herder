import Foundation
import Herder

/// The session last open in each project, remembered on this device by the project's id, so
/// switching to the project opens it again.
///
/// The rule: a project remembers a session only while it was open *inside that project*:
/// selected in the project's pane on the Mac and iPad, or pushed from the project's screen on
/// iPhone. A session opened anywhere else (the Board, Chats, a link, a notification) is not
/// remembered for its project until it is viewed inside it. Switching to a project opens the
/// session it remembers while that is still listed and not archived; otherwise the project
/// opens as it did before, on its list.
struct ProjectMemory {
    static let shared = ProjectMemory()

    let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    private static func key(_ projectId: String) -> String { "lastSession." + projectId }

    /// Remembers `session` as the one last open in `project`, if the project lists it.
    func remember(_ session: SessionKey, in project: ProjectGroup) {
        guard let projectId = project.projectId, Self.opens(session, in: project) else { return }
        defaults.set([session.hostId, session.sessionId], forKey: Self.key(projectId))
    }

    /// The session to open on switching to `project`: the one last open in it, while the
    /// project still lists it and it is not archived.
    func last(in project: ProjectGroup) -> SessionKey? {
        guard let projectId = project.projectId,
              let stored = defaults.stringArray(forKey: Self.key(projectId)), stored.count == 2 else { return nil }
        let session = SessionKey(hostId: stored[0], sessionId: stored[1])
        return Self.opens(session, in: project) ? session : nil
    }

    private static func opens(_ session: SessionKey, in project: ProjectGroup) -> Bool {
        project.live.contains { $0.key == session && $0.state != .archived }
    }

    /// The session to remember from the Projects tab's stack on iPhone, with the project it
    /// was opened in: the last session pushed over a project's screen.
    static func shown(in path: [NavRoute]) -> (projectId: String, session: SessionKey)? {
        guard case .project(let id) = path.first,
              let session = path.dropFirst().reversed().lazy.compactMap({ route -> SessionKey? in
                  if case .session(let key) = route { return key }
                  return nil
              }).first else { return nil }
        return (id, session)
    }

    /// Whether the Projects tab's stack just opened a project's screen, from the projects'
    /// list or another project, rather than come back to it from a session (where Back must
    /// stay on the list): the project's id, if so.
    static func entered(from old: [NavRoute], to new: [NavRoute]) -> String? {
        guard new.count == 1, case .project(let id) = new[0], old.first != new[0] else { return nil }
        return id
    }
}
