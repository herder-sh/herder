import Foundation
import Herder
import Network
import Observation

/// The paired machines and the state of every session they list, kept current from the
/// client's change notifications and one subscription per session, as the TUI keeps them.
@MainActor
@Observable
public final class Fleet {
    public let client: Client
    public private(set) var machines: [Machine] { didSet { listsChanged() } }
    private(set) var sessions: [SessionKey: SessionModel] = [:] { didSet { listsChanged() } }
    /// Provenance returned by forks made on this device during this app run.
    var forkOrigins: [SessionKey: ForkOrigin] = [:]
    /// Sessions being handed off, to the machine they go to; see `handOff`.
    var handoffs: [SessionKey: HostId] = [:]
    /// The last command a session refused, until its next command succeeds.
    private(set) var refusals: [SessionKey: String] = [:]
    /// Sessions whose archive the machine is working on; the lists show them archived already.
    private(set) var archiving: Set<SessionKey> = [] { didSet { refreshLists() } }
    /// An archive the machine refused, for the user to decide on.
    var archiveRefusal: ArchiveRefusal?
    /// The session the user is typing a new title for.
    var renaming: SessionKey?
    /// A short note about something that just finished, shown briefly.
    var toast: Toast?
    /// Each session's open shells, kept while the app runs; see `SessionTerminals`.
    var accountLogins: [HostId: TerminalConnection] = [:]
    @ObservationIgnored var sessionTerminals: [SessionKey: SessionTerminals] = [:]
    @ObservationIgnored private var subscriptions: [SessionKey: Task<Void, Never>] = [:]
    /// Images of user messages, fetched once: by attachment id.
    private(set) var attachments: [String: Data] = [:]
    /// Project icons the machines found in their clones, by the icon's hash.
    private(set) var projectIcons: [String: Data] = [:]
    @ObservationIgnored private var fetching: Set<String> = []
    @ObservationIgnored fileprivate var previousAverage: [HostId: UInt32?] = [:]
    /// Each machine's connection changes since the app opened, oldest first.
    private(set) var connectionLog: [HostId: [ConnectionChange]] = [:]
    /// Each machine's ping round trips since the app opened, oldest first.
    private(set) var roundTrips: [HostId: [RoundTrip]] = [:]
    /// The sessions that finished a turn since this device last opened them.
    private(set) var done: DoneSessions { didSet { listsChanged() } }
    /// What the lists show now. Replaced only when it differs, so the views that show it
    /// redraw when a list changes, not on every token a session streams.
    private(set) var lists = Lists()
    /// The rebuild of `lists` waiting to run; see `listsChanged`.
    @ObservationIgnored private var listsRefresh: Task<Void, Never>?
    /// The sessions shown on screen now, whose turns ending are seen.
    @ObservationIgnored private var watching: Set<SessionKey> = []

    /// `doneFile` keeps the done sessions across launches; `nil` keeps them in memory.
    public init(client: Client, doneFile: URL? = nil) {
        self.client = client
        machines = client.machines()
        done = DoneSessions(file: doneFile)
        refreshLists()
    }

    /// Replaces the machines without a client change, for tests.
    func setMachinesForTesting(_ machines: [Machine]) {
        self.machines = machines
        refreshLists()
    }

    /// Rebuilds the lists at most ten times a second. Opening the app replays every session's
    /// events, thousands at once; rebuilding on each kept the main thread busy until the
    /// system killed the app.
    private func listsChanged() {
        guard listsRefresh == nil else { return }
        listsRefresh = Task {
            try? await Task.sleep(for: .milliseconds(100))
            guard !Task.isCancelled else { return }
            refreshLists()
        }
    }

    private func refreshLists() {
        listsRefresh?.cancel()
        listsRefresh = nil
        let lists = Lists(machines: machines, sessions: sessions, done: done.keys, archiving: archiving)
        if lists != self.lists { self.lists = lists }
    }

    /// A session view shows `key`: it is seen, and stays seen while shown.
    func watch(_ key: SessionKey) {
        watching.insert(key)
        done.remove(key)
    }

    /// A session view stopped showing `key`.
    func unwatch(_ key: SessionKey) {
        watching.remove(key)
    }

    /// Follows the client's changes until it stops; runs for as long as the fleet is shown.
    public func follow() async {
        // Subscribe before reading, so a change between `init` and now is not missed.
        let changes = client.changes()
        let network = Task { await self.wakeOnNetworkChanges() }
        defer { network.cancel() }
        update(client.machines())
        while await changes.next() {
            update(client.machines())
        }
    }

    private func update(_ machines: [Machine]) {
        self.machines = machines
        fetchProjectIcons(machines)
        for machine in machines {
            recordRoundTrip(machine)
            var log = connectionLog[machine.hostId] ?? []
            guard log.last?.state != machine.connection else { continue }
            log.append(ConnectionChange(at: .now, state: machine.connection))
            connectionLog[machine.hostId] = Array(log.suffix(100))
        }
        for machine in machines {
            for head in machine.sessions where !head.queue.isEmpty {
                sessions[SessionKey(hostId: machine.hostId, sessionId: head.sessionId)]?.settle(head.queue)
            }
        }
        // A copy another copy stands for is not followed: a vault's copy of every session would
        // stream each one twice.
        let listed = Set(machines.flatMap { machine in
            machine.sessions.map { SessionKey(hostId: machine.hostId, sessionId: $0.sessionId) }
        }).subtracting(Lists.shadowed(machines))
        for key in listed where subscriptions[key] == nil {
            subscriptions[key] = Task { await self.stream(key) }
        }
        for (key, task) in subscriptions where !listed.contains(key) {
            task.cancel()
            subscriptions[key] = nil
            sessions[key] = nil
            done.remove(key)
        }
    }

    /// Folds a session's updates, cached state first, until it is no longer listed.
    private func stream(_ key: SessionKey) async {
        guard let subscription = try? client.subscribeSession(hostId: key.hostId, sessionId: key.sessionId)
        else { return }
        sessions[key] = sessions[key] ?? SessionModel(key: key)
        // Releasing the subscription when this returns unsubscribes.
        while let update = await subscription.next(), !Task.isCancelled {
            guard let before = sessions[key] else { break }
            sessions[key]?.apply(update)
            observe(key, before: before)
        }
    }

    /// Notes what an update did to `key`'s status, from the session as it was before it.
    func observe(_ key: SessionKey, before: SessionModel) {
        guard let after = sessions[key]?.status else { return }
        done.observe(key, before: before.status, after: after, wasLoaded: before.loaded,
                     open: watching.contains(key))
    }

    /// Sends a command about a session; a refusal is kept to show with it.
    func send(_ command: CommandBody, about key: SessionKey) async {
        do {
            _ = try await client.send(hostId: key.hostId, command: command)
            refusals[key] = nil
        } catch {
            refusals[key] = describe(error)
        }
    }

    func answer(_ request: PendingRequest, allow: Bool) async {
        let key = request.session.key
        await send(
            .answerApproval(sessionId: key.sessionId, approvalId: request.requestId, decision: allow ? .allow : .deny),
            about: key)
    }

    func answer(_ request: PendingRequest, with answer: Answer) async {
        let key = request.session.key
        await send(.answerQuestion(sessionId: key.sessionId, questionId: request.requestId, answer: answer), about: key)
    }

    /// The provider new sessions start on: the project's default account's, else Claude when the
    /// machine has a Claude account, else the first there is.
    func defaultProvider(on hostId: HostId, projectId: String?) -> Provider? {
        guard let machine = machines.first(where: { $0.hostId == hostId }) else { return nil }
        let preferred = machine.projects.first { $0.projectId == projectId }?.defaultAccount
        if let account = machine.accounts.first(where: { $0.accountId == preferred }) { return account.provider }
        if machine.accounts.contains(where: { $0.provider == "claude" }) { return "claude" }
        return machine.accounts.first?.provider
    }

    /// The account a new session runs on, so nobody has to pick one: the project's default
    /// account when it fits the provider, else the provider's account with the most room left.
    func defaultAccount(on hostId: HostId, projectId: String?, provider: Provider?) -> Account? {
        guard let machine = machines.first(where: { $0.hostId == hostId }) else { return nil }
        let preferred = machine.projects.first { $0.projectId == projectId }?.defaultAccount
        if let account = machine.accounts.first(where: { $0.accountId == preferred }),
           provider == nil || account.provider == provider {
            return account
        }
        return machine.accounts
            .filter { provider == nil || $0.provider == provider }
            .min { busiest($0) < busiest($1) }
    }

    private func busiest(_ account: Account) -> Double {
        account.usage.map(\.usedPercent).max() ?? 0
    }

    /// Creates a session, in `repo` or `projectId`, or a chat; prompts it when a prompt is
    /// given, and returns it.
    func createSession(
        on hostId: HostId, repo: String?, projectId: String?, chat: Bool = false, accountId: AccountId, model: String,
        mode: PermissionMode, prompt: String, images: [Herder.Image] = [], files: [PromptFile] = []
    ) async throws -> SessionKey {
        let result = try await client.send(
            hostId: hostId,
            command: .createSession(
                repo: repo, projectId: projectId, chat: chat, branch: nil, accountId: accountId, provider: nil,
                model: model.isEmpty ? nil : model, permissionMode: mode, failoverPin: nil))
        guard case .sessionCreated(let sessionId) = result else {
            throw HerderError.Local(detail: "the machine did not create a session")
        }
        let key = SessionKey(hostId: hostId, sessionId: sessionId)
        if !prompt.isEmpty || !images.isEmpty || !files.isEmpty {
            // Through the outbox, as any prompt: the machine journals the first one only once
            // its CLI is up, and till then the new session would show no turns.
            sessions[key] = sessions[key] ?? SessionModel(key: key)
            await submit(prompt, images: images, files: files, to: key)
        }
        return key
    }

    /// Sends what the user typed: the answer to the session's oldest question when one is
    /// pending, else a prompt, queued behind the turn when one runs, as the TUI does.
    func submit(_ text: String, images: [Herder.Image] = [], files: [PromptFile] = [], to key: SessionKey) async {
        guard let session = sessions[key] else { return }
        if let question = session.questions.first, images.isEmpty, files.isEmpty {
            await send(.answerQuestion(sessionId: key.sessionId, questionId: question.id, answer: .text(text: text)),
                       about: key)
            return
        }
        let outgoing = Outgoing(text: text, images: images, files: files)
        sessions[key]?.outbox.append(outgoing)
        await send(.sendPrompt(sessionId: key.sessionId, text: text, images: images, files: files), about: key)
        if let index = sessions[key]?.outbox.firstIndex(where: { $0.id == outgoing.id }) {
            sessions[key]?.outbox[index].state = refusals[key].map(Outgoing.State.failed) ?? .delivered
        }
        // The queue may have listed it before the machine answered.
        sessions[key]?.settle(queue(of: key))
    }

    /// The prompts waiting in a session's queue on its machine, in the order they will run.
    func queue(of key: SessionKey) -> [QueuedPrompt] {
        machines.first { $0.hostId == key.hostId }?.sessions.first { $0.sessionId == key.sessionId }?.queue ?? []
    }

    /// Drops a queued prompt without running it.
    func removeQueued(_ promptId: PromptId, from key: SessionKey) async {
        await editQueue(.removeQueued(sessionId: key.sessionId, promptId: promptId), of: key)
    }

    /// Moves a queued prompt just before another, or to the end.
    func moveQueued(_ promptId: PromptId, before: PromptId?, in key: SessionKey) async {
        await editQueue(.moveQueued(sessionId: key.sessionId, promptId: promptId, before: before), of: key)
    }

    /// Runs a queued prompt next, stopping the running turn; the rest keep their order.
    func sendQueuedNow(_ promptId: PromptId, in key: SessionKey) async {
        await editQueue(.sendQueuedNow(sessionId: key.sessionId, promptId: promptId), of: key)
    }

    /// Merges queued prompts into the first of them, so they run as one turn; the machine joins
    /// their texts, images and files, which only it holds.
    func mergeQueued(_ promptIds: [PromptId], in key: SessionKey) async {
        await editQueue(.mergeQueued(sessionId: key.sessionId, promptIds: promptIds), of: key)
    }

    /// Sends a queue edit; a prompt that started meanwhile is refused as such.
    private func editQueue(_ command: CommandBody, of key: SessionKey) async {
        do {
            _ = try await client.send(hostId: key.hostId, command: command)
            refusals[key] = nil
        } catch HerderError.Rejected(let info) where info.code == .conflict {
            refusals[key] = "That message has already started."
        } catch {
            refusals[key] = describe(error)
        }
    }

    /// Brings an archived session back, then sends the prompt; a refusal is shown with it.
    func unarchiveAndSubmit(_ text: String, images: [Herder.Image], files: [PromptFile] = [], to key: SessionKey) async {
        await send(.unarchiveSession(sessionId: key.sessionId), about: key)
        guard refusals[key] == nil else { return }
        await submit(text, images: images, files: files, to: key)
    }

    /// Fetches a user message's image once; views read it from `attachments`.
    func fetchAttachment(_ id: AttachmentId, of key: SessionKey) async {
        guard attachments[id] == nil, !fetching.contains(id) else { return }
        fetching.insert(id)
        defer { fetching.remove(id) }
        if case .attachment(_, let data)? = try? await client.send(
            hostId: key.hostId, command: .getAttachment(sessionId: key.sessionId, attachmentId: id)) {
            attachments[id] = data
        }
    }

    /// A project's icon, once fetched from a machine that has one.
    func projectIcon(_ projectId: ProjectId?) -> ProjectIconImage? {
        Self.icon(of: projectId, on: machines, fetched: projectIcons)
    }

    /// The project's icon every device shows: an uploaded icon before one found in a clone,
    /// then the machine with the lowest id, with that machine's background for it; without a
    /// fetched icon, the background of the lowest-id machine that sets one. Choosing by id keeps
    /// the choice from hanging on the order this device paired its machines in.
    nonisolated static func icon(
        of projectId: ProjectId?, on machines: [Machine], fetched: [String: Data]
    ) -> ProjectIconImage? {
        guard let projectId else { return nil }
        let listed = machines.sorted { $0.hostId < $1.hostId }
            .compactMap { machine in machine.projects.first { $0.projectId == projectId } }
        let iconed = listed.compactMap { project -> (uploaded: Bool, image: ProjectIconImage)? in
            project.icon.flatMap { fetched[$0] }
                .map { (project.iconUploaded, ProjectIconImage(data: $0, background: project.iconBackground)) }
        }
        return (iconed.first(where: \.uploaded) ?? iconed.first)?.image
            ?? listed.compactMap(\.iconBackground).first.map { ProjectIconImage(background: $0) }
    }

    /// Fetches each icon the machines list and this app has not got, once per hash.
    private func fetchProjectIcons(_ machines: [Machine]) {
        for machine in machines where machine.connection == .connected {
            for project in machine.projects {
                guard let icon = project.icon, projectIcons[icon] == nil, !fetching.contains(icon) else { continue }
                fetching.insert(icon)
                Task {
                    defer { fetching.remove(icon) }
                    if case .projectIcon(let hash, _, let data)? = try? await client.send(
                        hostId: machine.hostId, command: .getProjectIcon(projectId: project.projectId)) {
                        projectIcons[hash] = data
                    }
                }
            }
        }
    }

    /// A folder on a machine, to pick a repository; owners only.
    func listDirectory(_ path: String, on hostId: HostId) async throws -> (path: String, entries: [DirectoryEntry]) {
        guard case .directory(let path, let entries) = try await client.send(hostId: hostId, command: .listDirectory(path: path))
        else { throw HerderError.Local(detail: "the machine did not list the folder") }
        return (path, entries)
    }

    /// Registers a repository on a machine as a project; owners only.
    func addProject(_ path: String, on hostId: HostId) async throws -> ProjectId {
        guard case .projectAdded(let projectId) = try await client.send(hostId: hostId, command: .addProject(path: path))
        else { throw HerderError.Local(detail: "the machine did not add the project") }
        return projectId
    }

    /// Clones a repository, a git URL or GitHub `owner/repo`, into a new folder on a machine, or
    /// when `path` is nil into the machine's projects folder, and registers the clone as a
    /// project; owners only.
    func cloneProject(_ url: String, into path: String?, on hostId: HostId) async throws -> ProjectId {
        guard case .projectAdded(let projectId) = try await client.send(hostId: hostId, command: .cloneProject(url: url, path: path))
        else { throw HerderError.Local(detail: "the machine did not clone the project") }
        return projectId
    }

    /// The folder a machine scans for projects and clones them into; owners only.
    func projectsDir(on hostId: HostId) async throws -> String {
        guard case .settings(let settings, _, _, _) = try await client.send(hostId: hostId, command: .getSettings)
        else { throw HerderError.Local(detail: "the machine did not send its settings") }
        return settings.projects.dir
    }

    /// Replaces a project's settings on a machine; owners only.
    func setProjectSettings(
        _ projectId: ProjectId, on hostId: HostId, name: String, mode: PermissionMode?, account: AccountId?,
        setupCommand: String?, iconBackground: String?
    ) async throws {
        _ = try await client.send(hostId: hostId, command: .setProjectSettings(
            projectId: projectId, name: name, defaultPermissionMode: mode, defaultAccount: account,
            setupCommand: setupCommand, iconBackground: iconBackground))
    }

    /// Uploads a PNG as a project's icon on every connected machine this device owns that
    /// lists it, so every device shows the same one, or clears the uploads when `png` is nil
    /// so the machines find one in the clone again. The project lists that follow carry the
    /// new icon hash, which `fetchProjectIcons` fetches.
    func setProjectIcon(_ projectId: ProjectId, png: Data?) async throws {
        let icon = png.map { Herder.Image(mediaType: "image/png", data: $0) }
        for (machine, _) in ownedMachines(listing: projectId) {
            _ = try await client.send(hostId: machine.hostId, command: .setProjectIcon(projectId: projectId, icon: icon))
        }
    }

    /// Sets a project's name and the colour its tile fills with on every connected machine
    /// this device owns that lists it, so every device shows the same, keeping each machine's
    /// other settings for the project.
    func setProjectAppearance(_ projectId: ProjectId, name: String, background: String?) async throws {
        for (machine, project) in ownedMachines(listing: projectId)
        where project.name != name || project.iconBackground != background {
            try await setProjectSettings(projectId, on: machine.hostId, name: name, mode: project.defaultPermissionMode,
                                         account: project.defaultAccount, setupCommand: project.setupCommand,
                                         iconBackground: background)
        }
    }

    /// The connected machines this device owns that list the project, with their listing.
    private func ownedMachines(listing projectId: ProjectId) -> [(Machine, Project)] {
        machines.compactMap { machine in
            guard machine.role == .owner, machine.connection == .connected,
                  let project = machine.projects.first(where: { $0.projectId == projectId }) else { return nil }
            return (machine, project)
        }
    }

    /// Stops a machine managing a project; its clones stay on disk. The machine refuses while
    /// the project has sessions that are not archived.
    func removeProject(_ projectId: ProjectId, on hostId: HostId) async throws {
        _ = try await client.send(hostId: hostId, command: .removeProject(projectId: projectId))
    }

    /// Drops a prompt that failed to send.
    func discard(_ outgoing: Outgoing, from key: SessionKey) {
        sessions[key]?.outbox.removeAll { $0.id == outgoing.id }
    }

    func interrupt(_ key: SessionKey) async {
        await send(.interrupt(sessionId: key.sessionId), about: key)
    }

    func setMode(_ mode: PermissionMode, of key: SessionKey) async {
        await send(.setPermissionMode(sessionId: key.sessionId, mode: mode), about: key)
    }

    /// Moves a session to an account, a model, or both, as the TUI's switch dialog does: the
    /// same account only changes the model, another account of the same provider switches
    /// account (then model), and another provider's account switches provider.
    func switchSession(_ key: SessionKey, to account: Account, model: String) async {
        guard let session = sessions[key] else { return }
        let model = model.trimmingCharacters(in: .whitespaces)
        if account.accountId == session.accountId {
            guard !model.isEmpty, model != session.model else {
                refusals[key] = "Already on this account: enter a new model."
                return
            }
            await send(.setModel(sessionId: key.sessionId, model: model), about: key)
        } else if account.provider == session.provider {
            await send(.switchAccount(sessionId: key.sessionId, accountId: account.accountId), about: key)
            if !model.isEmpty, refusals[key] == nil {
                await send(.setModel(sessionId: key.sessionId, model: model), about: key)
            }
        } else {
            await send(.switchProvider(sessionId: key.sessionId, accountId: account.accountId,
                                       model: model.isEmpty ? nil : model), about: key)
        }
    }

    /// Archives a session: the lists show it archived, with its Undo toast, while the machine
    /// works on it. A refusal, such as a running turn, brings it back and goes to
    /// `archiveRefusal` to show.
    func archive(_ key: SessionKey) async {
        guard !archiving.contains(key) else { return }
        let title = sessions[key]?.title ?? "Session"
        archiving.insert(key)
        defer { archiving.remove(key) }
        let archived = Toast(text: "Archived “\(title)”", undo: key)
        toast = archived
        do {
            _ = try await client.send(hostId: key.hostId, command: .archiveSession(sessionId: key.sessionId))
            refusals[key] = nil
        } catch {
            if toast == archived { toast = nil }
            archiveRefusal = ArchiveRefusal(key: key, title: title, reason: describe(error))
        }
    }

    /// Sets a session's title; a refusal, such as an empty title, shows as a toast.
    func rename(_ key: SessionKey, to title: String) async {
        do {
            _ = try await client.send(hostId: key.hostId,
                                      command: .renameSession(sessionId: key.sessionId, title: title))
            refusals[key] = nil
        } catch {
            toast = Toast(text: "Could not rename: \(describe(error))", failed: true)
        }
    }

    /// The sessions of `before`, an earlier `archiving`, that the machine has archived since:
    /// done archiving and not refused.
    func archived(since before: Set<SessionKey>) -> Set<SessionKey> {
        before.subtracting(archiving).filter { archiveRefusal?.key != $0 }
    }

    /// Brings an archived session back, from a toast's Undo.
    func unarchive(_ key: SessionKey) async {
        toast = nil
        do {
            _ = try await client.send(hostId: key.hostId, command: .unarchiveSession(sessionId: key.sessionId))
        } catch {
            toast = Toast(text: describe(error))
        }
    }

    /// Pairs with every machine a `herder://pair` link names: one result per machine, in the
    /// link's order.
    @discardableResult
    public func pair(link: String) async throws -> [PairResult] {
        let results = try await client.pair(link: link)
        update(client.machines())
        return results
    }

    /// A link that pairs another device with every connected machine, as this device's user
    /// with its role on each.
    public func share() async throws -> SharedLink {
        try await client.share()
    }

    func rename(_ hostId: HostId, to name: String) throws {
        try client.rename(hostId: hostId, name: name)
        update(client.machines())
    }

    /// Connects to a machine at `addresses`, first to last in order of preference.
    func setAddresses(_ hostId: HostId, to addresses: [String]) throws {
        try client.setAddresses(hostId: hostId, addresses: addresses)
        update(client.machines())
    }

    /// Puts `address` first and reconnects the machine now, so the connection moves to it.
    /// Throws when it did not answer, though another address may have.
    func connect(_ hostId: HostId, through address: String) async throws {
        let addresses = machines.first { $0.hostId == hostId }?.addresses ?? []
        try setAddresses(hostId, to: [address] + addresses.filter { $0 != address })
        let used = try await client.reconnect(hostId: hostId)
        update(client.machines())
        if used != address {
            throw HerderError.Local(detail: "\(address) did not answer; connected through \(used).")
        }
    }

    func forget(_ hostId: HostId) throws {
        try client.forget(hostId: hostId)
        update(client.machines())
    }

    /// The app is in the foreground: reconnects now and replaces dead connections.
    public func wake() {
        client.wake()
    }

    /// Wakes the client whenever the network changes, as when the Wi-Fi changes or a VPN such
    /// as UniFi Teleport or Tailscale comes up or goes down: a lost connection is retried at
    /// once instead of after its backoff, and one that is up moves to an address that comes
    /// before its own once that answers.
    private func wakeOnNetworkChanges() async {
        var previous: NWPath?
        for await path in NWPathMonitor() {
            defer { previous = path }
            // The first path is the network as it is now, not a change.
            guard let previous, path != previous, path.status == .satisfied else { continue }
            client.wake()
        }
    }

    /// The app went to the background: saves the offline cache and stops retrying.
    public func suspend() {
        client.suspend()
    }
}

/// The profile this device keeps its pairings and offline cache in.
public enum Profile {
    case opened(Fleet)
    case failed(String)

    /// Opens the profile in `directory`, creating it if needed.
    @MainActor
    public static func open(at directory: URL, client name: String) -> Profile {
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            return .opened(Fleet(
                client: try Client.open(configDir: directory.path, client: name),
                doneFile: directory.appendingPathComponent("cache/done.json")))
        } catch {
            return .failed(describe(error))
        }
    }

    /// Opens the app's profile in Application Support.
    @MainActor
    public static func openDefault() -> Profile {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        #if os(iOS)
        let platform = "ios"
        #else
        let platform = "macos"
        #endif
        return open(
            at: support.appendingPathComponent("herder", isDirectory: true),
            client: "herder-\(platform)/\(version ?? "dev")")
    }
}

/// A machine's connection changing state.
/// One ping's round trip, as the client core reported it.
struct RoundTrip: Hashable {
    let at: Date
    let milliseconds: UInt32
}

extension Fleet {
    /// Keeps a machine's newest round trip; a report that changed neither the last nor the
    /// average round trip is the same ping again.
    fileprivate func recordRoundTrip(_ machine: Machine) {
        guard let rtt = machine.quality.lastRttMs else { return }
        var log = roundTrips[machine.hostId] ?? []
        if let last = log.last, last.milliseconds == rtt,
           previousAverage[machine.hostId] == machine.quality.averageRttMs { return }
        previousAverage[machine.hostId] = machine.quality.averageRttMs
        log.append(RoundTrip(at: .now, milliseconds: rtt))
        roundTrips[machine.hostId] = Array(log.suffix(240))
    }
}

/// An archive the machine refused, and why.
struct ArchiveRefusal: Identifiable {
    let key: SessionKey
    let title: String
    let reason: String
    var id: SessionKey { key }
}

/// A brief note, with Undo when the session it is about can be brought back.
struct Toast: Identifiable, Equatable {
    let id = UUID()
    let text: String
    var undo: SessionKey?
    /// Says what went wrong, rather than what finished.
    var failed = false
}

struct ConnectionChange: Hashable {
    let at: Date
    let state: ConnectionState
}
