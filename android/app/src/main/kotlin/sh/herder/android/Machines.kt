package sh.herder.android

import java.time.Duration
import java.time.Instant
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.channelFlow
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import sh.herder.ffi.CiStatus
import sh.herder.ffi.Client
import sh.herder.ffi.ConnectionState
import sh.herder.ffi.Event
import sh.herder.ffi.EventBody
import sh.herder.ffi.FailoverSettings
import sh.herder.ffi.FleetHost
import sh.herder.ffi.HerderException
import sh.herder.ffi.Machine
import sh.herder.ffi.Mergeable
import sh.herder.ffi.PermissionMode
import sh.herder.ffi.PrState
import sh.herder.ffi.Project
import sh.herder.ffi.PullRequest
import sh.herder.ffi.ReviewStatus
import sh.herder.ffi.SessionHead
import sh.herder.ffi.SessionStatus
import sh.herder.ffi.SessionUpdate

/** What the app shows: the client's machines, or why its profile could not be opened. */
sealed interface Profile {
    /** The machines, and what each listed session's subscription said of it. */
    data class Open(val machines: List<Machine>, val summaries: Map<SessionKey, Summary> = emptyMap()) : Profile

    data class Failed(val message: String) : Profile
}

/** The client's machines, now and after every change, until the client stops. */
fun Client.machinesFlow(): Flow<List<Machine>> = flow {
    changes().use { changes ->
        emit(machines())
        while (changes.next()) emit(machines())
    }
}.conflate()

/** A session's updates, cached state first, until the client or the machine stops. */
fun Client.updates(key: SessionKey): Flow<SessionUpdate> = flow {
    val subscription = try {
        subscribeSession(key.hostId, key.sessionId)
    } catch (_: HerderException) {
        // The machine is gone; the next machine list drops the session.
        return@flow
    }
    // Closing the subscription unsubscribes.
    subscription.use {
        while (true) emit(it.next() ?: break)
    }
}

/**
 * The machines of [machines], each listed session followed through [updates] for what the lists
 * show and its list entry lacks. Emits only when the lists could change: a session's streaming
 * deltas leave its summary as it was.
 */
fun fleetFlow(
    machines: Flow<List<Machine>>,
    updates: (SessionKey) -> Flow<SessionUpdate>,
): Flow<Profile.Open> = channelFlow {
    val state = MutableStateFlow<Profile.Open?>(null)
    launch { state.filterNotNull().collect { send(it) } }
    val followed = mutableMapOf<SessionKey, Job>()
    machines.collect { list ->
        val listed = keys(list)
        state.update { Profile.Open(list, it?.summaries.orEmpty().filterKeys(listed::contains)) }
        followed.entries.removeAll { (key, job) -> (key !in listed).also { gone -> if (gone) job.cancel() } }
        for (key in listed - followed.keys) {
            followed[key] = launch {
                updates(key).collect { update ->
                    state.update { open ->
                        // An update can outrun the machine list that drops its session.
                        if (open == null || key !in keys(open.machines)) return@update open
                        val before = open.summaries[key] ?: Summary()
                        val after = before.applied(update)
                        if (after == before) open else open.copy(summaries = open.summaries + (key to after))
                    }
                }
            }
        }
    }
}

/** A machine's connection, in words. */
fun ConnectionState.label(): String = when (this) {
    ConnectionState.Connected -> "connected"
    ConnectionState.Connecting -> "connecting"
    is ConnectionState.Disconnected -> error
}

/** How many machines are connected, for the title bar. */
fun summary(machines: List<Machine>): String {
    val connected = machines.count { it.connection == ConnectionState.Connected }
    return when (machines.size) {
        0 -> ""
        connected -> "all connected"
        else -> "$connected of ${machines.size} connected"
    }
}

/** A machine with only a name, a connection and sessions, for previews and tests. */
internal fun machine(
    hostId: String,
    name: String,
    connection: ConnectionState,
    sessions: List<SessionHead> = emptyList(),
) = Machine(
    hostId = hostId,
    name = name,
    addresses = emptyList(),
    fingerprint = "",
    connection = connection,
    role = null,
    sessions = sessions,
    hosts = emptyList(),
    projects = emptyList(),
    accounts = emptyList(),
    failover = FailoverSettings(pin = false),
    terminals = emptyList(),
    resources = null,
    sessionUsage = emptyMap(),
)

/** A session's list entry, idle and on no vault host, for previews and tests. */
internal fun head(sessionId: String, projectId: String?) = SessionHead(
    sessionId = sessionId,
    hostId = null,
    headSeq = 0u,
    status = SessionStatus.IDLE,
    parent = null,
    task = null,
    projectId = projectId,
    accountId = "claude-main",
    childrenNeedYou = 0u,
)

/** An update of [sessionId] with [bodies], for previews and tests. */
internal fun update(sessionId: String, vararg bodies: EventBody) = SessionUpdate(
    events = bodies.mapIndexed { seq, body ->
        Event(sessionId = sessionId, seq = seq.toULong() + 1u, at = "1970-01-01T00:00:00Z", by = null, body = body)
    },
    streaming = emptyList(),
)

/** A session's first event, for previews and tests. */
internal fun created(repo: String, branch: String): EventBody = EventBody.SessionCreated(
    repo = repo,
    worktree = "/srv/worktrees/$branch",
    branch = branch,
    provider = "claude",
    accountId = "claude-main",
    model = "claude-opus",
    permissionMode = PermissionMode.ASK,
    parent = null,
    task = null,
    maxChildren = null,
    failoverPin = null,
)

/** A pull request, for previews and tests. */
internal fun pr(number: Int, state: PrState, ci: CiStatus) = PullRequest(
    number = number.toULong(),
    url = "https://github.com/org/app/pull/$number",
    title = "PR $number",
    headBranch = null,
    state = state,
    ci = ci,
    review = ReviewStatus.NONE,
    mergeable = Mergeable.CLEAN,
)

/**
 * A task tree on `box` and a lone session on `nas`, both in `app`; and a vault with two hosts,
 * one offline since two hours and five minutes before [now]. For previews and tests.
 */
internal fun sampleFleet(now: Instant): Profile.Open {
    fun child(id: String, task: String, status: SessionStatus) =
        head(id, "github.com/org/app").copy(parent = "s2", task = task, status = status)
    val box = machine(
        "h1", "box", ConnectionState.Connected,
        listOf(
            head("s2", "github.com/org/app").copy(status = SessionStatus.RUNNING, childrenNeedYou = 1u),
            child("s3", "write the tests", SessionStatus.NEEDS_YOU),
            child("s4", "document it", SessionStatus.ERROR),
            // Not resolved to a project yet.
            head("s5", null),
        ),
    ).copy(
        projects = listOf(
            Project(
                projectId = "github.com/org/app",
                name = "App",
                paths = listOf("/srv/app"),
                defaultPermissionMode = null,
                defaultAccount = null,
                setupCommand = null,
            ),
        ),
    )
    val nas = machine(
        "h2", "nas", ConnectionState.Disconnected("connection refused"),
        listOf(head("s1", "github.com/org/app")),
    )
    val vault = machine(
        "v", "vault", ConnectionState.Connected,
        listOf(
            head("s6", "github.com/org/web").copy(hostId = "devbox", status = SessionStatus.RUNNING),
            head("s7", "github.com/org/web").copy(hostId = "laptop", status = SessionStatus.WAITING_FOR_CAPACITY),
        ),
    ).copy(
        hosts = listOf(
            FleetHost(hostId = "devbox", hostName = "devbox", online = true, lastSeen = now.toString()),
            FleetHost(
                hostId = "laptop",
                hostName = "laptop",
                online = false,
                lastSeen = now.minus(Duration.ofMinutes(2 * 60 + 5)).toString(),
            ),
        ),
    )
    val summaries = listOf(
        Triple("h1", "s2", created("/srv/app", "herder/api")),
        Triple("h1", "s3", created("/srv/app", "herder/api-tests")),
        Triple("h1", "s4", created("/srv/app", "herder/api-docs")),
        Triple("h1", "s5", created("/srv/scratch", "herder/try")),
        Triple("h2", "s1", created("/srv/app", "herder/fix-login")),
        Triple("v", "s6", created("/srv/web", "herder/login")),
        Triple("v", "s7", created("/srv/web", "herder/docs")),
    ).associate { (host, id, created) -> SessionKey(host, id) to Summary().applied(update(id, created)) }
        .toMutableMap()
    val api = SessionKey("h1", "s2")
    summaries[api] = summaries.getValue(api).applied(
        update(
            "s2",
            EventBody.PrLinked(pr(12, PrState.OPEN, CiStatus.PASSING)),
            EventBody.PrLinked(pr(9, PrState.MERGED, CiStatus.PASSING)),
        ),
    )
    val tests = SessionKey("h1", "s3")
    summaries[tests] = summaries.getValue(tests).applied(
        update("s3", EventBody.PrLinked(pr(14, PrState.DRAFT, CiStatus.FAILING))),
    )
    return Profile.Open(listOf(box, nas, vault), summaries)
}
