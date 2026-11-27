#if os(macOS)
import AppKit
#endif
import CoreGraphics
import Foundation
import Herder
import Network
import Observation
import OSLog

/// The paired machines and the state of every session they list, kept current from the
/// client's change notifications and one subscription per session, as the TUI keeps them.
@MainActor
@Observable
public final class Fleet {
    public let client: Client
    /// The machines. A view that reads them redraws when a machine's own state changes, such as
    /// an account, a setting or its connection, not when a session it lists moves on, as a
    /// running turn's does several times a second, nor what a hub counts of the sessions it
    /// keeps, which moves with them: read `liveMachines` for those. Both return the machines as
    /// they are now.
    public var machines: [Machine] {
        _ = ownChanges
        return current
    }
    /// The machines, for a view that shows the sessions they list: it redraws when either
    /// changes.
    var liveMachines: [Machine] {
        _ = ownChanges
        _ = headChanges
        return current
    }
    @ObservationIgnored private var current: [Machine]
    /// Which copy is listed of each session a host and a hub both list. It moves from one to
    /// the other as machines connect and drop, and nothing a session shows may go with it; see
    /// `source(for:)`.
    private var copies = SessionCopies()
    /// The copies no longer followed whose state is kept: one a view shows, and one another
    /// copy stands for, until that copy shows as much. See `release(listing:)`.
    @ObservationIgnored private var retained: Set<SessionKey> = []
    /// The journals of copies followed again that are replayed beside what was kept of them;
    /// see `replay(_:of:into:)`.
    @ObservationIgnored private var replays: [SessionKey: SessionModel] = [:]
    /// Bumped when the machines' own state changes, and when only what moves with their
    /// sessions does; see `machines`.
    private var ownChanges = 0
    private var headChanges = 0
    /// Each followed session's state in a box of its own; the dictionary itself changes only as
    /// sessions come and go. See `session(_:)` and `sessions`.
    private var boxes: [SessionKey: SessionBox] = [:]
    /// Provenance returned by forks made on this device during this app run.
    var forkOrigins: [SessionKey: ForkOrigin] = [:]
    /// Sessions being handed off, to the machine they go to; see `handOff`.
    var handoffs: [SessionKey: HostId] = [:]
    /// The last command a session refused, until its next command succeeds.
    private(set) var refusals: [SessionKey: String] = [:]
    /// Sessions whose archive the machine is working on; the lists show them archived already.
    private(set) var archiving: Set<SessionKey> = [] { didSet { refreshLists() } }
    /// The archives made from this app that an undo can still take back, each with its command:
    /// an undo can come before the machine has answered it.
    @ObservationIgnored private var archives: [SessionKey: Task<Void, any Error>] = [:]
    /// An archive the machine refused, for the user to decide on.
    var archiveRefusal: ArchiveRefusal?
    /// The session the user is typing a new title for.
    var renaming: SessionKey?
    /// A short note about something that just finished, shown briefly.
    var toast: Toast?
    /// The sessions sent from a draft that their machine has not created yet; see `start`.
    private(set) var starting: [StartingSession] = []
    /// A session started away from its list, as Fix with AI starts one, for the window to show.
    var revealed: SessionKey?
    /// A claim a hub setup asks you to approve, for the window to show its approval sheet.
    var claiming: ApprovalRequest?
    /// The hub setups waiting on their claim's approval; see `claim(_:)`.
    @ObservationIgnored var claims: [ApprovalRequest: CheckedContinuation<HostId, Error>] = [:]
    /// The installs, logins and AWS sign-ins this device started; see `Activity`.
    var activity = Activity()
    /// Every owned machine's bots, as Bot Settings last asked; see `FleetBots`.
    var bots = FleetBots()
    /// Each session's open shells, kept while the app runs; see `SessionTerminals`.
    @ObservationIgnored var sessionTerminals: [SessionKey: SessionTerminals] = [:]
    @ObservationIgnored private var subscriptions: [SessionKey: Task<Void, Never>] = [:]
    /// How far each followed session's journal went, by its machine's listing, when this device
    /// began to follow it, until what it holds of it reaches there. What arrives up to there is
    /// history, in however many updates: this device holds the journals of a machine's newest
    /// sessions only, and the machine sends every other one from its start on each launch.
    @ObservationIgnored private var history: [SessionKey: UInt64] = [:]
    /// Images of user messages, fetched once: by attachment id.
    private(set) var attachments: [String: Data] = [:]
    /// Project icons the machines list, read into pictures, by the icon's hash.
    private(set) var projectIcons: [String: CGImage] = [:]
    /// Icons fetched that are no picture this app reads, by hash, so they are not fetched again.
    @ObservationIgnored private var unreadableIcons: Set<String> = []
    @ObservationIgnored private var fetching: Set<String> = []
    @ObservationIgnored fileprivate var previousAverage: [HostId: UInt32?] = [:]
    /// When the client last told of a change, or the app last woke it; see `checkHeard`.
    @ObservationIgnored private var heard = ContinuousClock.now
    /// Whether the app is in the background, where iOS freezes it and nothing is heard.
    @ObservationIgnored private var asleep = false
    /// Whether the client pings its machines, so that silence means it reaches none of them;
    /// the demo pings nothing.
    @ObservationIgnored var hearsPings = true
    /// Each machine's connection changes since the app opened, oldest first.
    private(set) var connectionLog: [HostId: [ConnectionChange]] = [:]
    /// Each machine's ping round trips since the app opened, oldest first.
    private(set) var roundTrips: [HostId: [RoundTrip]] = [:]
    /// The sessions that finished a turn since this device last opened them.
    private(set) var done: DoneSessions { didSet { listsChanged() } }
    /// What the lists show now. Each list is replaced only when it differs, so a view that shows
    /// one redraws when that list changes, not on every token a session streams.
    let lists = ShownLists()
    /// The machines' latest usage summaries, per period; see `refreshUsage`.
    var usageCache = UsageCache()
    /// Where each machine last said it backs up, so its page shows that at once; see
    /// `backupLink(of:)`.
    var backupLinks: [HostId: BackupLink] = [:]
    /// The rebuild of `lists` waiting to run; see `listsChanged`.
    @ObservationIgnored private var listsRefresh: Task<Void, Never>?
    /// The sessions shown on screen now, whose turns ending are seen.
    @ObservationIgnored private var watching: Set<SessionKey> = []

    /// `doneFile` keeps the done sessions across launches; `nil` keeps them in memory.
    public init(client: Client, doneFile: URL? = nil) {
        self.client = client
        let machines = client.machines()
        current = machines
        done = DoneSessions(file: doneFile)
        copies = Lists.copies(machines)
        refreshLists()
    }

    /// Replaces the machines without a client change, for tests.
    func setMachinesForTesting(_ machines: [Machine]) {
        set(shown(machines))
        refreshLists()
    }

    /// `machines`, as the client lists them, with the connections to show. The client rides
    /// out a short drop itself, so a machine it lists as disconnected has been away a while:
    /// it shows offline through the retries that follow, until one connects, rather than turn
    /// to "connecting" and back with each.
    private func shown(_ machines: [Machine]) -> [Machine] {
        var offline: [HostId: ConnectionState] = [:]
        for machine in current {
            if case .disconnected = machine.connection { offline[machine.hostId] = machine.connection }
        }
        return machines.map { machine in
            guard machine.connection == .connecting, let connection = offline[machine.hostId] else { return machine }
            var machine = machine
            machine.connection = connection
            return machine
        }
    }

    /// Takes the machines the client lists, telling the views that read them only what changed.
    private func set(_ machines: [Machine]) {
        guard machines != current else { return }
        let ownChanged = machines.map(Self.own) != current.map(Self.own)
        current = machines
        let copies = Lists.copies(machines, keeping: self.copies.shown)
        if copies != self.copies { self.copies = copies }
        // `liveMachines` reads both.
        if ownChanged { ownChanges += 1 } else { headChanges += 1 }
        listsChanged()
    }

    /// A machine without what moves as its sessions do: their heads, and a hub's counts of
    /// what it keeps of each host's, which change with every event the hub takes in.
    private static func own(_ machine: Machine) -> Machine {
        var own = machine
        own.sessions = []
        own.hosts = machine.hosts.map { host in
            var host = host
            host.lastSeen = ""
            host.usage = nil
            return host
        }
        own.hub = machine.hub.map { hub in
            HubStatus(sessions: 0, events: 0, storageBytes: 0, hosts: hub.hosts.map { replication in
                HostReplication(hostId: replication.hostId, sessions: 0, events: 0, lastEventAt: nil, lagMs: nil)
            })
        }
        return own
    }

    /// Folds an update into a session as its subscription would, for tests.
    func applyForTesting(_ update: SessionUpdate, to key: SessionKey) {
        track(key)
        change(key) { $0.apply(update) }
    }

    /// Holds an attachment's bytes as though fetched, for tests.
    func setAttachmentForTesting(_ id: AttachmentId, _ data: Data) { attachments[id] = data }

    /// Rebuilds the lists at most ten times a second. Opening the app replays every session's
    /// events, thousands at once; rebuilding on each kept the main thread busy until the
    /// system killed the app.
    private func listsChanged() {
        guard listsRefresh == nil else { return }
        listsRefresh = Task {
            try? await Task.sleep(for: .milliseconds(100))
            await scrollEnded()
            guard !Task.isCancelled else { return }
            refreshLists()
        }
    }

    #if os(iOS)
    /// The main run loop's mode while the user drags or flings a scroll view.
    static let scrollMode = RunLoop.Mode.tracking

    /// Whether the user is dragging or flinging a scroll view.
    static var scrolling: Bool { RunLoop.main.currentMode == scrollMode }
    #else
    /// When the last scroll-wheel event arrived: a mouse wheel's, a trackpad's or its momentum's.
    /// A Mac scroll leaves the run loop in its default mode, so its events tell it instead.
    static var lastScroll = Date.distantPast
    /// How long after its last event a scroll counts as over.
    static let scrollSettles: TimeInterval = 0.15
    private static let scrollMonitor: Any? = NSEvent.addLocalMonitorForEvents(matching: .scrollWheel) { event in
        MainActor.assumeIsolated { lastScroll = .now }
        return event
    }

    /// Whether the user is scrolling, or was a moment ago.
    static var scrolling: Bool { Date.now.timeIntervalSince(lastScroll) < scrollSettles }
    #endif

    /// The longest a scroll holds the fleet's changes back: under a long drag or a slow fling
    /// they still arrive, a batch at a time.
    static let scrollHold = Duration.milliseconds(500)

    /// Returns once no scroll is in progress, or once one has held the changes back for
    /// `scrollHold`: at once unless one is. The fleet's changes wait for it, so a list scrolls
    /// without the views around it drawing again on every event a session streams; the latest
    /// applies as the scroll ends. What needs the user does not wait: see `receive(_:for:)`.
    func scrollEnded() async {
        #if os(macOS)
        _ = Self.scrollMonitor
        #endif
        let until = ContinuousClock.now + Self.scrollHold
        while isScrolling(), ContinuousClock.now < until {
            try? await Task.sleep(for: .milliseconds(25))
        }
    }

    /// Whether a scroll is in progress, for this fleet's `scrollEnded`: the app's, unless a test
    /// stands in a scroll of its own, which then holds back this fleet's changes alone.
    var isScrolling: @MainActor () -> Bool = { Fleet.scrolling }

    private func refreshLists() {
        listsRefresh?.cancel()
        listsRefresh = nil
        // A copy listed since a moment ago shows what was kept of the one it stands for, until
        // its own journal has come as far: a row keeps its title, state and place meanwhile.
        var sessions = self.sessions
        for kept in retained {
            guard let standIn = copies.standIns[kept], let model = sessions[kept],
                  model.showsMore(than: sessions[standIn]) else { continue }
            sessions[standIn] = model.keyed(standIn)
        }
        // A session done on one copy is done on the copy listed.
        var done = self.done.keys
        for key in self.done.keys {
            if let standIn = copies.standIns[key] { done.insert(standIn) }
        }
        lists.show(Lists(machines: current, sessions: sessions, done: done, archiving: archiving, copies: copies))
    }

    /// The copy whose state a view of `key` shows. A session a hub replicates is listed from
    /// its host or from the hub as they connect and drop, and an open session follows it
    /// there: it shows the copy that has folded the most of the journal, so it never steps back
    /// or empties while the other copy loads; of two as far along, the listed one.
    func source(for key: SessionKey) -> SessionKey {
        var source = copies.standIns[key] ?? key
        for copy in [key] + (copies.others[key] ?? []) where copy != source {
            if let model = boxes[copy]?.model, model.showsMore(than: boxes[source]?.model) { source = copy }
        }
        return source
    }

    /// Whether `key` is a hub's copy of a session another host runs: only that host takes
    /// what is sent to the session, and the hub refuses it.
    func isReplica(_ key: SessionKey) -> Bool {
        let head = machines.first { $0.hostId == key.hostId }?.sessions.first { $0.sessionId == key.sessionId }
        return head?.hostId.map { $0 != key.hostId } ?? false
    }

    /// The copy the lists show of `key`'s session: itself, unless another stands for it.
    func listed(_ key: SessionKey) -> SessionKey { copies.standIns[key] ?? key }

    /// One session's state. A view that reads it redraws when that session changes only, not
    /// on every token another session streams.
    func session(_ key: SessionKey) -> SessionModel? { boxes[key]?.model }

    /// `key`'s transcript with each turn's work folded, as its view shows it; observed as
    /// `session(_:)` is, and built again only when the session or its queue changes.
    func transcript(_ key: SessionKey) -> [TranscriptBlock] {
        boxes[key]?.transcript(queue: queue(of: key)) ?? []
    }

    /// Every session's state. A view that reads it redraws whenever any session changes: read
    /// one with `session(_:)` where that is all a view needs.
    var sessions: [SessionKey: SessionModel] { boxes.mapValues(\.model) }

    /// Changes `key`'s state, when it is followed.
    /// Closes the session's standing failure card, on this device only.
    func dismissFailure(_ key: SessionKey) { change(key) { $0.dismissFailure() } }

    private func change(_ key: SessionKey, _ body: (inout SessionModel) -> Void) {
        guard let box = boxes[key] else { return }
        body(&box.model)
        listsChanged()
    }

    /// Follows `key`'s state from now on, when it isn't yet.
    private func track(_ key: SessionKey) {
        if boxes[key] == nil { boxes[key] = SessionBox(SessionModel(key: key)) }
    }

    /// A session view shows `key`: it is seen, and stays seen while shown.
    func watch(_ key: SessionKey) {
        watching.insert(key)
        done.remove(key)
        for copy in copies.others[key] ?? [] { done.remove(copy) }
    }

    /// Whether a session view shows `key` or another copy of its session.
    private func isWatched(_ key: SessionKey) -> Bool {
        watching.contains(key) || (copies.others[key] ?? []).contains { watching.contains($0) }
    }

    /// A session view stopped showing `key`.
    func unwatch(_ key: SessionKey) {
        watching.remove(key)
    }

    /// Follows the client's changes until it stops; runs for as long as the fleet is shown.
    public func follow() async {
        await joinFakeFleet()
        #if os(macOS)
        _ = Self.scrollMonitor
        #endif
        // Subscribe before reading, so a change between `init` and now is not missed.
        let changes = client.changes()
        let network = Task { await self.wakeOnNetworkChanges() }
        defer { network.cancel() }
        let silence = Task { await self.watchSilence() }
        defer { silence.cancel() }
        update(client.machines())
        while await changes.next() {
            heard = .now
            await changed()
        }
    }

    /// How long the client may tell of nothing before the machines it lists as connected show
    /// as offline. It pings each connected machine every 10 s and tells of every answer and
    /// every missed one, so a client silent for this long reaches none of them, whatever they
    /// read: a phone showed two machines connected for hours after both connections were gone.
    static let silence = Duration.seconds(30)

    private func watchSilence() async {
        while (try? await Task.sleep(for: .seconds(5))) != nil { checkHeard() }
    }

    /// Shows each machine that reads connected as offline once the client has told of nothing
    /// for `silence` while the app was in the foreground, and wakes the client to replace their
    /// connections. The next change the client tells of shows the machines as it lists them.
    func checkHeard(now: ContinuousClock.Instant = .now) {
        guard hearsPings, !asleep, now - heard >= Self.silence,
              current.contains(where: { $0.connection == .connected }) else { return }
        let quiet = String(describing: now - heard)
        Self.logger.error("no change from the client for \(quiet, privacy: .public): its connected machines show as offline")
        set(current.map { machine in
            guard machine.connection == .connected else { return machine }
            var machine = machine
            machine.connection = .disconnected(error: "no answer for \(Int(Self.silence.components.seconds)) s")
            return machine
        })
        refreshLists()
        client.wake()
    }

    private static let logger = Logger(subsystem: "sh.herder", category: "connection")

    /// Takes in the client's lists after a change to them. They are read away from the main
    /// actor: a read copies every session every machine lists out of the client core, and they
    /// change with every list, resource report and ping answer of every machine.
    func changed() async {
        var machines = await Self.machines(of: client)
        // A machine connecting or going away shows at once, scroll or not.
        if reconnects(shown(machines)) {
            update(machines)
            refreshLists()
            return
        }
        // What a scroll held back is stale by its end.
        if isScrolling() {
            await scrollEnded()
            machines = await Self.machines(of: client)
        }
        update(machines)
    }

    @concurrent private nonisolated static func machines(of client: Client) async -> [Machine] {
        client.machines()
    }

    /// Whether `machines` connect or disconnect one of the machines shown.
    private func reconnects(_ machines: [Machine]) -> Bool {
        let connections = { (machines: [Machine]) in
            Dictionary(machines.map { ($0.hostId, $0.connection) }) { first, _ in first }
        }
        return connections(machines) != connections(current)
    }

    func update(_ reported: [Machine]) {
        let listedNone = current.allSatisfy(\.sessions.isEmpty)
        let machines = shown(reported)
        set(machines)
        // While nothing was listed, as on a launch, at once: the first sessions are the offline
        // cache's, which the Board shows in place of the splash. From then on, throttled, so a
        // burst of machine changes does not rebuild the lists under a scroll.
        if listedNone { refreshLists() }
        settleUpdates()
        fetchProjectIcons(machines)
        // The connections as they are: the machine's page shows each drop, however short.
        for machine in reported {
            recordRoundTrip(machine)
            var log = connectionLog[machine.hostId] ?? []
            guard log.last?.state != machine.connection else { continue }
            log.append(ConnectionChange(at: .now, state: machine.connection))
            connectionLog[machine.hostId] = Array(log.suffix(100))
        }
        for machine in machines {
            for head in machine.sessions where !head.queue.isEmpty {
                change(SessionKey(hostId: machine.hostId, sessionId: head.sessionId)) { $0.settle(head.queue) }
            }
        }
        // A copy another copy stands for is not followed: a hub's copy of every session would
        // stream each one twice.
        let heads = Dictionary(machines.flatMap { machine in
            machine.sessions.map { (SessionKey(hostId: machine.hostId, sessionId: $0.sessionId), $0.headSeq) }
        }) { first, _ in first }
        let followed = Set(heads.keys).subtracting(copies.shadowed)
        for key in followed where subscriptions[key] == nil {
            history[key] = heads[key]
            // What is kept of a copy followed again stays as it is while its journal replays.
            if retained.remove(key) != nil, boxes[key]?.model.loaded == true { replays[key] = SessionModel(key: key) }
            subscriptions[key] = Task { await self.stream(key) }
        }
        for (key, task) in subscriptions where !followed.contains(key) {
            task.cancel()
            subscriptions[key] = nil
            history[key] = nil
            replays[key] = nil
            retained.insert(key)
        }
        release(listing: heads)
    }

    /// Drops the state of the copies no longer followed, each once nothing shows it. A session
    /// its machine no longer lists is gone, unless a view still shows it: that keeps what it
    /// shows. A copy another stands for goes once that copy shows as much: until then the lists
    /// and an open session show the session from what is kept here. Dropping it as it left the
    /// lists blanked the open session, and emptied its row, on every drop of a connection.
    private func release(listing heads: [SessionKey: UInt64]) {
        for key in retained where !isWatched(key) {
            if heads[key] != nil, let standIn = copies.standIns[key], let kept = boxes[key]?.model,
               kept.showsMore(than: boxes[standIn]?.model) { continue }
            retained.remove(key)
            boxes[key] = nil
            // A copy another stands for keeps its mark: the session is done on either.
            if heads[key] == nil { done.remove(key) }
            listsChanged()
        }
    }

    /// Folds a session's updates, cached state first, until it is no longer listed.
    private func stream(_ key: SessionKey) async {
        guard let subscription = try? client.subscribeSession(hostId: key.hostId, sessionId: key.sessionId)
        else { return }
        track(key)
        // Releasing the subscription when this returns unsubscribes.
        while let update = await subscription.next(), !Task.isCancelled {
            guard await receive(update, for: key) else { break }
        }
    }

    /// The events from which an update is long: it folds away from the main actor. A session's
    /// first update is its whole cached journal, and a machine sends what a session did while
    /// the app was away at once: thousands of events, for hundreds of sessions on a launch.
    /// Folded here, they held the main thread for seconds, in the first moments the app shows.
    /// A turn streaming arrives an event or a few at a time, which fold here faster than they
    /// would get away and back.
    static let longUpdate = 64

    /// Folds `update` into `key`'s state once no scroll holds it back, at once when it needs the
    /// user; false once the session is no longer followed.
    func receive(_ update: SessionUpdate, for key: SessionKey) async -> Bool {
        let urgent = update.needsUser
        if !urgent { await scrollEnded() }
        // A subscription cancelled meanwhile is of a copy no longer followed, whose state may
        // be kept: nothing more folds into it.
        guard !Task.isCancelled, let box = boxes[key] else { return false }
        var update = update
        if replays[key] != nil {
            guard let rest = await replay(update, of: key, into: box) else { return !Task.isCancelled }
            update = rest
        }
        // Only what `observe` needs: holding the whole session would copy its transcript on
        // every update.
        let before = (status: box.model.status, loaded: box.model.loaded)
        if update.events.count >= Self.longUpdate {
            let revision = box.revision
            let folded = await Self.fold(update, into: box.model)
            guard !Task.isCancelled, boxes[key] === box else { return false }
            if box.revision == revision {
                box.model = folded
                listsChanged()
            } else {
                // The session changed here meanwhile, by a prompt sent or a card closed, which
                // what was folded lacks: the update folds over that instead.
                change(key) { $0.apply(update) }
            }
        } else {
            change(key) { $0.apply(update) }
        }
        observe(key, before: before.status, wasLoaded: before.loaded)
        // The request lists show it at once too.
        if urgent { refreshLists() }
        return true
    }

    /// Folds `update` into the journal `key` replays, up to where what is kept of it ends: a
    /// copy followed again gets its journal from the start, which folded into what is kept
    /// would show every turn twice, and folded into an empty session would empty the open
    /// one until it had loaded. Once the replay is as far as what is kept it takes its place,
    /// with nothing on screen changing, and the rest of `update` is returned to fold as any
    /// other; `nil` while there is more to replay.
    private func replay(_ update: SessionUpdate, of key: SessionKey, into box: SessionBox) async -> SessionUpdate? {
        guard var replay = replays[key] else { return update }
        let kept = box.model.lastSeq
        let seen = Array(update.events.prefix { $0.seq <= kept })
        if seen.count >= Self.longUpdate {
            replay = await Self.fold(SessionUpdate(events: seen, streaming: []), into: replay)
            guard !Task.isCancelled, boxes[key] === box, replays[key] != nil else { return nil }
        } else {
            replay.apply(SessionUpdate(events: seen, streaming: []))
        }
        // What is kept may have moved on meanwhile, by a prompt sent: it is read again.
        guard replay.lastSeq >= box.model.lastSeq else {
            replays[key] = replay
            return nil
        }
        replays[key] = nil
        replay.keepLocal(of: box.model)
        box.model = replay
        return SessionUpdate(events: Array(update.events.dropFirst(seen.count)), streaming: update.streaming)
    }

    /// `model` with `update` folded in, away from the main actor.
    @concurrent
    private nonisolated static func fold(_ update: SessionUpdate, into model: SessionModel) async -> SessionModel {
        var model = model
        model.apply(update)
        return model
    }

    /// Notes what an update did to `key`'s status, from its status and load before it.
    func observe(_ key: SessionKey, before: SessionStatus, wasLoaded: Bool) {
        guard let box = boxes[key] else { return }
        var news = wasLoaded
        if let head = history[key] {
            // An update that ends within the journal's history leaves the session where it once
            // stood: a turn it starts or ends says nothing of whether the session is done now.
            guard (box.model.timeline.last?.seq ?? 0) >= head else { return }
            // The one that ends the history shows where the session stands, as no news.
            history[key] = nil
            news = false
        }
        let after = box.model.status
        done.observe(key, before: before, after: after, wasLoaded: news, open: isWatched(key))
        // Brought back, from here or elsewhere: its archive is no longer there to undo.
        if before == .archived, after != .archived { archives[key] = nil }
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

    /// Allows `request`, an agent's `store_secret` of `secret`, storing it in `set`: a set the
    /// hub lacks is created first.
    func storeSecret(_ request: PendingRequest, _ secret: SecretStoreRequest, in set: String) async {
        let key = request.session.key
        do {
            let hub = try await ownedHub(of: key.hostId, creating: set).hub
            guard case .secretToStore(let value) = try await client.send(
                hostId: key.hostId, command: .getSecretToStore(sessionId: key.sessionId, approvalId: request.requestId))
            else { throw HerderError.Local(detail: "the machine did not hand over the value") }
            _ = try await client.send(
                hostId: hub.hostId,
                command: .putSecret(set: set, name: secret.name, description: secret.description, value: value, assertion: nil))
            _ = try await client.send(
                hostId: key.hostId,
                command: .confirmSecretStored(sessionId: key.sessionId, approvalId: request.requestId, set: set, assertion: nil))
            refusals[key] = nil
        } catch {
            refusals[key] = describe(error)
        }
    }

    /// Answers `request`, an agent's `request_secret` of `secret`, with `value` the owner typed,
    /// stored in `set`: a set the hub lacks is created first. The value goes from this device
    /// to the host's hub alone, as its owner, never to the host, its journal or the agent,
    /// which learns only that it was stored. Whether the hub stored it; a refusal is kept to
    /// show on the card.
    func supplySecret(_ request: PendingRequest, _ secret: SecretRequest, value: SecretInput, in set: String) async -> Bool {
        let key = request.session.key
        do {
            if let problem = SecretRequestAsk.problem(value) { throw HerderError.Local(detail: problem) }
            let (hub, held) = try await ownedHub(of: key.hostId, creating: set)
            let description = SecretRequestAsk.description(secret, set: set, held: held)
            _ = try await client.send(
                hostId: hub.hostId,
                command: .putSecret(set: set, name: secret.name, description: description, value: value.text, assertion: nil))
            _ = try await client.send(
                hostId: key.hostId,
                command: .confirmSecretStored(sessionId: key.sessionId, approvalId: request.requestId, set: set, assertion: nil))
            refusals[key] = nil
            return true
        } catch {
            refusals[key] = describe(error)
            return false
        }
    }

    func answer(_ request: PendingRequest, with answer: Answer) async {
        let key = request.session.key
        await send(
            .answerQuestion(sessionId: key.sessionId, questionId: request.requestId, answer: answer, images: [], files: []),
            about: key)
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
    /// account when it fits the provider, else the provider's account with the most room left,
    /// a fallback-only one only once every other is used up, as the machine itself picks.
    func defaultAccount(on hostId: HostId, projectId: String?, provider: Provider?) -> Account? {
        guard let machine = machines.first(where: { $0.hostId == hostId }) else { return nil }
        let preferred = machine.projects.first { $0.projectId == projectId }?.defaultAccount
        if let account = machine.accounts.first(where: { $0.accountId == preferred }),
           provider == nil || account.provider == provider {
            return account
        }
        return pick(machine.accounts.filter { provider == nil || $0.provider == provider })
    }

    /// Of `candidates`, the one with the most room left, a fallback-only one only once every
    /// other is used up.
    func pick(_ candidates: [Account]) -> Account? {
        let rotating = candidates.filter { !$0.fallback && busiest($0) < 100 }
        return (rotating.isEmpty ? candidates : rotating).min { busiest($0) < busiest($1) }
    }

    private func busiest(_ account: Account) -> Double {
        account.usage.map(\.usedPercent).max() ?? 0
    }

    /// Creates a session, in `repo` or `projectId`, or a chat; prompts it when a prompt is
    /// given, and returns it.
    func createSession(
        on hostId: HostId, repo: String?, projectId: String?, chat: Bool = false, accountId: AccountId, model: String,
        mode: PermissionMode, prompt: String, images: [Herder.Image] = [], files: [PromptFile] = [],
        created: () -> Void = {}
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
        track(key)
        created()
        if !prompt.isEmpty || !images.isEmpty || !files.isEmpty {
            // Through the outbox, as any prompt: the machine journals the first one only once
            // its CLI is up, and till then the new session would show no turns. The outbox
            // takes it before `submit` first waits, so the session is never listed without it.
            await submit(prompt, images: images, files: files, to: key)
        }
        return key
    }

    /// Starts a session from a draft in the background, so the draft is free to close and
    /// another to start meanwhile: it is listed in `starting` until its machine creates it, when
    /// the session's own row, titled by the prompt on its way, takes its place. A failure puts
    /// the prompt back in the draft, kept in `drafts`, and is toasted.
    func start(
        _ draft: Draft, accountId: AccountId, model: String, mode: PermissionMode, prompt: String,
        images: [Herder.Image] = [], files: [PromptFile] = [], drafts: PromptDrafts = .shared
    ) -> Task<Result<SessionKey, any Error>, Never> {
        let pending = StartingSession(draft: draft, prompt: prompt)
        starting.append(pending)
        return Task {
            defer { starting.removeAll { $0.id == pending.id } }
            do {
                return .success(try await createSession(
                    on: draft.hostId, repo: draft.createArguments.repo, projectId: draft.createArguments.projectId,
                    chat: draft.isChat, accountId: accountId, model: model, mode: mode, prompt: prompt,
                    images: images, files: files, created: { starting.removeAll { $0.id == pending.id } }))
            } catch {
                drafts.restore(PromptDrafts.Content(text: prompt, images: images, files: files, draft: draft),
                               for: draft.key)
                let name = machines.first { $0.hostId == draft.hostId }?.name ?? draft.hostId
                toast = .failure("Could not start the session on \(name)", error)
                return .failure(error)
            }
        }
    }

    /// Sends what the user typed, with its attachments: the answer to the session's oldest
    /// question when one is pending, else a prompt, queued behind the turn when one runs, as
    /// the TUI does.
    func submit(_ text: String, images: [Herder.Image] = [], files: [PromptFile] = [], to key: SessionKey) async {
        guard let session = session(key) else { return }
        if let question = session.questions.first {
            await send(
                .answerQuestion(
                    sessionId: key.sessionId, questionId: question.id, answer: .text(text: text), images: images,
                    files: files),
                about: key)
            return
        }
        let outgoing = Outgoing(text: text, images: images, files: files)
        change(key) { $0.outbox.append(outgoing) }
        await send(.sendPrompt(sessionId: key.sessionId, text: text, images: images, files: files), about: key)
        let state = refusals[key].map(Outgoing.State.failed) ?? .delivered
        let queue = queue(of: key)
        change(key) { session in
            if let index = session.outbox.firstIndex(where: { $0.id == outgoing.id }) {
                session.outbox[index].state = state
            }
            // The queue may have listed it before the machine answered.
            session.settle(queue)
        }
    }

    /// The prompts waiting in a session's queue on its machine, in the order they will run.
    func queue(of key: SessionKey) -> [QueuedPrompt] {
        liveMachines.first { $0.hostId == key.hostId }?.sessions.first { $0.sessionId == key.sessionId }?.queue ?? []
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
        of projectId: ProjectId?, on machines: [Machine], fetched: [String: CGImage]
    ) -> ProjectIconImage? {
        guard let projectId else { return nil }
        let listed = machines.sorted { $0.hostId < $1.hostId }
            .compactMap { machine in machine.projects.first { $0.projectId == projectId } }
        let iconed = listed.compactMap { project -> (uploaded: Bool, image: ProjectIconImage)? in
            project.icon.flatMap { fetched[$0] }
                .map { (project.iconUploaded, ProjectIconImage(picture: $0, background: project.iconBackground)) }
        }
        return (iconed.first(where: \.uploaded) ?? iconed.first)?.image
            ?? listed.compactMap(\.iconBackground).first.map { ProjectIconImage(background: $0) }
    }

    /// Fetches each icon the machines list and this app has not got, once per hash.
    private func fetchProjectIcons(_ machines: [Machine]) {
        for machine in machines where machine.connection == .connected {
            for project in machine.projects {
                guard let icon = project.icon, projectIcons[icon] == nil, !unreadableIcons.contains(icon),
                      !fetching.contains(icon) else { continue }
                fetching.insert(icon)
                Task {
                    defer { fetching.remove(icon) }
                    if case .projectIcon(let hash, _, let data)? = try? await client.send(
                        hostId: machine.hostId, command: .getProjectIcon(projectId: project.projectId)) {
                        if let picture = ProjectIconImage.picture(data) {
                            projectIcons[hash] = picture
                        } else {
                            unreadableIcons.insert(hash)
                        }
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

    /// The folder a machine clones projects into; owners only.
    func projectsDir(on hostId: HostId) async throws -> String {
        guard case .settings(let settings, _, _, _) = try await client.send(hostId: hostId, command: .getSettings)
        else { throw HerderError.Local(detail: "the machine did not send its settings") }
        return settings.projects.dir
    }

    /// The MCP servers every session on a machine gets next to herder's own, in order; owners
    /// only.
    func mcpServers(on hostId: HostId) async throws -> [McpServerSettings] {
        guard case .settings(let settings, _, _, _) = try await client.send(hostId: hostId, command: .getSettings)
        else { throw HerderError.Local(detail: "the machine did not send its settings") }
        return settings.mcpServers
    }

    /// Replaces the MCP servers on a machine, to add, change, remove or turn them on and off,
    /// keeping its other settings; owners only. Each session's agent gets them from its next
    /// start. The machine refuses an invalid server, or a name used twice or `herder`, with
    /// `badRequest`.
    func setMcpServers(_ servers: [McpServerSettings], on hostId: HostId) async throws {
        guard case .settings(var settings, _, _, _) = try await client.send(hostId: hostId, command: .getSettings)
        else { throw HerderError.Local(detail: "the machine did not send its settings") }
        settings.mcpServers = servers
        _ = try await client.send(hostId: hostId, command: .setSettings(settings: settings))
    }

    /// Replaces a project's settings on a machine; owners only.
    func setProjectSettings(
        _ projectId: ProjectId, on hostId: HostId, name: String, mode: PermissionMode?, account: AccountId?,
        setupCommand: String?, iconBackground: String?, privateArtifacts: Bool
    ) async throws {
        _ = try await client.send(hostId: hostId, command: .setProjectSettings(
            projectId: projectId, name: name, defaultPermissionMode: mode, defaultAccount: account,
            setupCommand: setupCommand, iconBackground: iconBackground, privateArtifacts: privateArtifacts))
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
                                         iconBackground: background, privateArtifacts: project.privateArtifacts)
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
        change(key) { $0.outbox.removeAll { $0.id == outgoing.id } }
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
        guard let session = session(key) else { return }
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

    /// Archives a session: the lists show it archived, with its Undo toast, saying `text` when
    /// given, while the machine works on it. A refusal, such as a running turn, brings it back
    /// and goes to `archiveRefusal` to show.
    func archive(_ key: SessionKey, toast text: String? = nil) async {
        guard !archiving.contains(key) else { return }
        let title = session(key)?.title ?? "Session"
        archiving.insert(key)
        defer { archiving.remove(key) }
        let archived = Toast(text: text ?? "Archived “\(title)”", undo: key)
        toast = archived
        let command = Task {
            _ = try await client.send(hostId: key.hostId, command: .archiveSession(sessionId: key.sessionId))
        }
        archives[key] = command
        do {
            try await command.value
            refusals[key] = nil
        } catch {
            archives[key] = nil
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
            toast = .failure("Could not rename", error)
        }
    }

    /// The sessions of `before`, an earlier `archiving`, that the machine has archived since:
    /// done archiving and not refused.
    func archived(since before: Set<SessionKey>) -> Set<SessionKey> {
        before.subtracting(archiving).filter { archiveRefusal?.key != $0 }
    }

    /// Undoes an archive made from this app, from its toast's Undo or ⌘Z, once the machine has
    /// made it: an undo can come while the machine still works on it, and before the session
    /// here says it is archived. Nothing is undone twice, nor an archive the machine refused,
    /// nor one of a session that has come back since.
    func undoArchive(_ key: SessionKey) async {
        guard let archive = archives.removeValue(forKey: key) else { return }
        if toast?.undo == key { toast = nil }
        do { try await archive.value } catch { return }
        await unarchive(key)
    }

    /// Brings an archived session back.
    func unarchive(_ key: SessionKey) async {
        archives[key] = nil
        toast = nil
        do {
            _ = try await client.send(hostId: key.hostId, command: .unarchiveSession(sessionId: key.sessionId))
        } catch {
            toast = .failure("Could not unarchive", error)
        }
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
        asleep = false
        heard = .now
        client.wake()
    }

    /// Wakes the client whenever the network changes, as when the Wi-Fi changes or a VPN such
    /// as UniFi Teleport or Tailscale comes up or goes down: a lost connection is retried at
    /// once instead of after its backoff, and one that is up is checked and replaced if it
    /// died with the network it used.
    ///
    /// A network that comes and goes reports a burst of paths, an interface or a route at a
    /// time, and each wake pings every connected machine and replaces the connections that do
    /// not answer in time: one wake follows the burst, once it has settled.
    private func wakeOnNetworkChanges() async {
        var previous: NWPath?
        var settling: Task<Void, Never>?
        defer { settling?.cancel() }
        for await path in NWPathMonitor() {
            defer { previous = path }
            // The first path is the network as it is now, not a change.
            guard let previous, path != previous else { continue }
            settling?.cancel()
            guard path.status == .satisfied else { continue }
            settling = Task {
                guard (try? await Task.sleep(for: Self.networkSettles)) != nil else { return }
                self.wake()
            }
        }
    }

    /// How long the network's path has to stay as it is before the client is woken for it.
    static let networkSettles = Duration.seconds(1)

    /// The app went to the background: saves the offline cache and stops retrying.
    public func suspend() {
        asleep = true
        client.suspend()
    }
}

/// The profile this device keeps its pairings and offline cache in.
public enum Profile {
    case opened(Fleet)
    case failed(String)

    /// Opens the profile in `directory`, creating it if needed. The done sessions are kept in
    /// `doneFile`, by default in the profile's cache. Your terminal and secret commands are
    /// signed with this device's passkey, behind Face ID.
    @MainActor
    public static func open(at directory: URL, client name: String, doneFile: URL? = nil) -> Profile {
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            return .opened(Fleet(
                client: try Client.openWithPasskey(configDir: directory.path, client: name, authenticator: FaceID()),
                doneFile: doneFile ?? directory.appendingPathComponent("cache/done.json")))
        } catch {
            return .failed(describe(error))
        }
    }

    /// Opens the app's profile. On the Mac it is the TUI's, `~/.config/herder`, so both show
    /// the same machines; on iOS, `herder` in Application Support. `HERDER_PROFILE` names a
    /// profile of its own, `herder-<name>` in Application Support, so profiling and tests keep
    /// off the real pairings. The done sessions stay in Application Support: they are the app's.
    @MainActor
    public static func openDefault() -> Profile {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        #if os(iOS)
        let platform = "ios"
        #else
        let platform = "macos"
        #endif
        let own = ProcessInfo.processInfo.environment["HERDER_PROFILE"].map { "herder-\($0)" }
        let app = support.appendingPathComponent(own ?? "herder", isDirectory: true)
        var directory = app
        #if os(macOS)
        if own == nil, let home = realHome() {
            directory = home.appendingPathComponent(".config/herder", isDirectory: true)
        }
        #endif
        let profile = open(
            at: directory,
            client: "herder-\(platform)/\(version ?? "dev")",
            doneFile: app.appendingPathComponent("cache/done.json"))
        Launch.mark(.profile)
        return profile
    }

    #if os(macOS)
    /// The user's home directory. In the sandbox `NSHomeDirectory()` is the app's container;
    /// the entitlements let the app into `~/.config/herder` of the real one.
    private static func realHome() -> URL? {
        guard let entry = getpwuid(getuid()), let dir = entry.pointee.pw_dir else { return nil }
        return URL(fileURLWithPath: String(cString: dir), isDirectory: true)
    }
    #endif
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
/// A session sent from a draft, while its machine creates it.
struct StartingSession: Identifiable, Equatable {
    let id = UUID()
    let draft: Draft
    let prompt: String
}

struct Toast: Identifiable, Equatable {
    let id = UUID()
    /// The headline, on one line in the toast.
    let text: String
    /// The whole error, shown when the toast is tapped.
    var detail: String?
    var undo: SessionKey?
    /// A file it saved, which Show in Finder reveals.
    var reveal: URL?
    /// Says what went wrong, rather than what finished. It stays until dismissed.
    var failed = false

    /// A failure: `headline` says what could not be done, the error why.
    static func failure(_ headline: String, _ error: any Error) -> Toast {
        Toast(text: headline, detail: describe(error), failed: true)
    }
}

struct ConnectionChange: Hashable {
    let at: Date
    let state: ConnectionState
}

extension SessionUpdate {
    /// Whether it asks the user something or reports a failed turn: it shows at once, even
    /// while a scroll holds the fleet's other changes back.
    var needsUser: Bool {
        events.contains { event in
            switch event.body {
            case .approvalRequested, .approvalEscalated, .questionAsked, .questionEscalated, .turnFailed: true
            default: false
            }
        }
    }
}
