package sh.herder.android

import java.time.Duration
import java.time.Instant
import java.time.OffsetDateTime
import java.time.format.DateTimeParseException
import sh.herder.ffi.CiStatus
import sh.herder.ffi.EventBody
import sh.herder.ffi.FleetHost
import sh.herder.ffi.HostId
import sh.herder.ffi.Machine
import sh.herder.ffi.Mergeable
import sh.herder.ffi.PrState
import sh.herder.ffi.ProjectId
import sh.herder.ffi.PullRequest
import sh.herder.ffi.ReviewStatus
import sh.herder.ffi.SessionHead
import sh.herder.ffi.SessionId
import sh.herder.ffi.SessionStatus
import sh.herder.ffi.SessionUpdate

// The session lists, as the TUI shows them, built from the client's machines and what each
// session's subscription said: grouped by project, each session labelled by where it runs, or
// by machine, a vault's sessions under the host they run on. Children come under their parent;
// a primary counts its children and how many of them need the user.
//
// A session's project is the `projectId` its daemon lists it with; until the daemon has
// resolved it, the local project of its repo on its machine.

/** A session of a machine; the key of everything per session. */
data class SessionKey(val hostId: HostId, val sessionId: SessionId)

/** What a session's events say that the lists show and its list entry does not. */
data class Summary(
    /** Whether its first update arrived. */
    val loaded: Boolean = false,
    /** Repository path on the host. */
    val repo: String = "",
    /** Branch the session works on now. */
    val branch: String = "",
    /** Pull requests linked to the session now, in the order they were linked. */
    val prs: List<PullRequest> = emptyList(),
    /** Provider it runs on now, for the model picker. */
    val provider: String = "",
    /** Model it runs on now, for the model picker. */
    val model: String = "",
) {
    /** This summary with [update] folded in. */
    fun applied(update: SessionUpdate): Summary {
        var repo = repo
        var branch = branch
        var provider = provider
        var model = model
        val prs = prs.toMutableList()
        for (event in update.events) {
            when (val body = event.body) {
                is EventBody.SessionCreated -> {
                    repo = body.repo
                    branch = body.branch
                    provider = body.provider
                    model = body.model
                }
                is EventBody.ModelSwitched -> model = body.model
                is EventBody.ProviderSwitched -> {
                    provider = body.provider
                    model = body.model
                }
                is EventBody.BranchCheckedOut -> branch = body.branch
                is EventBody.PrLinked -> prs.link(body.pr)
                is EventBody.PrUpdated -> prs.link(body.pr)
                is EventBody.PrUnlinked -> prs.removeAll { it.number == body.number }
                else -> {}
            }
        }
        return Summary(loaded = true, repo = repo, branch = branch, prs = prs, provider = provider, model = model)
    }
}

private fun MutableList<PullRequest>.link(pr: PullRequest) {
    val known = indexOfFirst { it.number == pr.number }
    if (known >= 0) this[known] = pr else add(pr)
}

/** How the session list groups sessions. */
enum class Grouping {
    /** Project → sessions, each labelled by where it runs. */
    Projects,

    /** Machine → sessions; a vault's, host → sessions. */
    Machines,
}

/** Whose sessions the list shows: what is selected in the machines list. */
sealed interface Scope {
    /** Every machine's. */
    data object All : Scope

    /** One machine's. */
    data class Machine(val hostId: HostId) : Scope

    /** The sessions a vault lists on one of its hosts. */
    data class Host(val vault: HostId, val host: HostId) : Scope
}

/** A heading and the sessions under it. */
data class Group(
    val title: String,
    val description: String,
    /** The state its sessions roll up to; `null` when it has none. */
    val status: SessionStatus?,
    val rows: List<SessionRow>,
)

/** One session in the list. */
data class SessionRow(
    val key: SessionKey,
    /** Nesting under its parent; 0 for a top-level session. */
    val depth: Int,
    val title: String,
    val status: SessionStatus,
    /** How many listed children it has. */
    val children: Int,
    /** How many of its children wait on the user; 0 when it has none listed. */
    val needYou: Int,
    /** Its pull requests, open ones first. */
    val prs: List<PullRequest>,
    /** Where it runs, while the list groups by project. */
    val place: String?,
    /** For a `moved` copy, the host the session went to. */
    val movedTo: String?,
) {
    /** What it rolls up as: needs you while a child does. */
    val attention: SessionStatus get() = if (needYou > 0) SessionStatus.NEEDS_YOU else status
}

/** Every listed session, for the subscriptions to follow. */
fun keys(machines: List<Machine>): Set<SessionKey> =
    machines.flatMapTo(mutableSetOf()) { machine -> machine.sessions.map { key(machine, it) } }

/** The models the app's sessions of [provider] run on, for the model picker, by name. */
fun recentModels(summaries: Map<SessionKey, Summary>, provider: String?): List<String> =
    summaries.values.filter { it.provider == provider && it.model.isNotEmpty() }.map { it.model }.distinct().sorted()

/** `count` sessions, in words. */
fun sessions(count: Int): String = if (count == 1) "1 session" else "$count sessions"

/** A vault's host's state: online, or offline with when the vault last heard from it. */
fun hostState(host: FleetHost, now: Instant): String {
    if (host.online) return "online"
    val lastSeen = try {
        OffsetDateTime.parse(host.lastSeen).toInstant()
    } catch (_: DateTimeParseException) {
        return "offline"
    }
    val minutes = Duration.between(lastSeen, now).seconds.coerceAtLeast(0) / 60
    val (days, hours, mins) = Triple(minutes / 1440, minutes / 60 % 24, minutes % 60)
    val ago = when {
        days > 0 -> "${days}d ${hours}h"
        hours > 0 -> "${hours}h ${mins}m"
        else -> "${mins}m"
    }
    return "offline · $ago ago"
}

/** What the machines list offers, in order: all machines, then each machine and its hosts. */
fun scopes(machines: List<Machine>): List<Scope> = buildList {
    if (machines.isNotEmpty()) add(Scope.All)
    for (machine in machines) {
        add(Scope.Machine(machine.hostId))
        machine.hosts.forEach { add(Scope.Host(machine.hostId, it.hostId)) }
    }
}

/** The title of the session list for [scope]. */
fun scopeTitle(machines: List<Machine>, scope: Scope): String = when (scope) {
    Scope.All -> "All machines"
    is Scope.Machine -> machines.find { it.hostId == scope.hostId }?.name.orEmpty()
    is Scope.Host -> machines.find { it.hostId == scope.vault }
        ?.hosts?.find { it.hostId == scope.host }?.hostName.orEmpty()
}

/** A status in words. */
fun SessionStatus.label(): String = when (this) {
    SessionStatus.IDLE -> "idle"
    SessionStatus.RUNNING -> "running"
    SessionStatus.WAITING_FOR_CAPACITY -> "waiting"
    SessionStatus.NEEDS_YOU -> "needs you"
    SessionStatus.ERROR -> "error"
    SessionStatus.ARCHIVED -> "archived"
    SessionStatus.MOVED -> "moved"
    SessionStatus.UNKNOWN -> "unknown"
}

/** A status as the TUI's glyph (docs/tui-design.md, 3.2). */
fun SessionStatus.glyph(): String = when (this) {
    SessionStatus.NEEDS_YOU -> "◉"
    SessionStatus.ERROR -> "✗"
    SessionStatus.RUNNING -> "●"
    SessionStatus.WAITING_FOR_CAPACITY -> "◌"
    SessionStatus.IDLE -> "○"
    SessionStatus.ARCHIVED -> "▪"
    SessionStatus.MOVED -> "→"
    SessionStatus.UNKNOWN -> "·"
}

/** Roll-up order: what a group of sessions shows is its highest. */
private fun SessionStatus.priority(): Int = when (this) {
    SessionStatus.NEEDS_YOU -> 6
    SessionStatus.ERROR -> 5
    SessionStatus.RUNNING -> 3
    SessionStatus.WAITING_FOR_CAPACITY -> 2
    SessionStatus.IDLE -> 1
    SessionStatus.ARCHIVED, SessionStatus.MOVED, SessionStatus.UNKNOWN -> 0
}

/** A PR: its number, then for a live one its checks and a `!` for a conflict or requested changes. */
fun badge(pr: PullRequest): String = buildString {
    append("#${pr.number}")
    if (pr.state == PrState.OPEN || pr.state == PrState.DRAFT) {
        append(
            when (pr.ci) {
                CiStatus.PASSING -> " ✓"
                CiStatus.FAILING -> " ✗"
                CiStatus.PENDING -> " …"
                CiStatus.NONE -> ""
            },
        )
        if (pr.mergeable == Mergeable.CONFLICTING || pr.review == ReviewStatus.CHANGES_REQUESTED) append('!')
    }
}

/** The pull requests a list shows, open ones first, each group in link order. */
private fun ordered(prs: List<PullRequest>): List<PullRequest> =
    prs.sortedBy { it.state == PrState.MERGED || it.state == PrState.CLOSED }

private fun key(machine: Machine, head: SessionHead) = SessionKey(machine.hostId, head.sessionId)

/** The name of a project no machine lists: the last segment of its id. */
private fun projectIdName(id: ProjectId): String = id.split('/', ':').lastOrNull { it.isNotEmpty() } ?: id

/** The lists of [machines], with what their sessions' subscriptions said. */
class Lists(
    private val machines: List<Machine>,
    private val summaries: Map<SessionKey, Summary>,
    /** For a narrow screen: only each branch's last part, and no machine names in headings. */
    private val compact: Boolean,
    /** What offline hosts' last-seen times are counted from. */
    private val now: Instant,
) {
    /** The sessions [scope] covers, grouped by [grouping]. */
    fun groups(scope: Scope, grouping: Grouping): List<Group> = when (grouping) {
        Grouping.Projects -> byProject(scope)
        Grouping.Machines -> byMachine(scope)
    }

    private fun byProject(scope: Scope): List<Group> {
        val projects = LinkedHashMap<ProjectId?, MutableList<SessionKey>>()
        for (machine in machines) {
            for (head in machine.sessions) {
                if (!inScope(machine, head, scope)) continue
                projects.getOrPut(projectOf(machine, head)) { mutableListOf() } += key(machine, head)
            }
        }
        val order = compareBy<Map.Entry<ProjectId?, List<SessionKey>>>(
            { it.key == null },
            { it.key?.let(::projectName).orEmpty() },
            { it.key.orEmpty() },
        )
        return projects.entries.sortedWith(order).map { (project, keys) ->
            var description = sessions(keys.size)
            if (!compact) {
                val on = machines.filter { machine -> keys.any { it.hostId == machine.hostId } }
                description += " · " + on.joinToString(", ") { it.name }
            }
            // Session ids are ULIDs, so they sort oldest first across machines too.
            val sorted = keys.sortedWith(compareBy({ it.sessionId }, { it.hostId }))
            group(
                title = project?.let(::projectName) ?: "No project yet",
                description = description,
                rows = forest(sorted, Grouping.Projects),
            )
        }
    }

    private fun byMachine(scope: Scope): List<Group> = buildList {
        for (machine in machines) {
            val shown = when (scope) {
                Scope.All -> true
                is Scope.Machine -> machine.hostId == scope.hostId
                is Scope.Host -> machine.hostId == scope.vault
            }
            if (!shown) continue
            fun on(host: HostId?) = machine.sessions
                .filter { host == null || it.hostId == host }
                .map { key(machine, it) }
            if (machine.hosts.isEmpty()) {
                // The daemon lists sessions oldest first.
                val keys = on(null)
                add(
                    group(
                        title = machine.name,
                        description = "${machine.connection.label()} · ${sessions(keys.size)}",
                        rows = forest(keys, Grouping.Machines),
                    ),
                )
                continue
            }
            // A vault: each host, then the sessions that run on it.
            for (host in machine.hosts) {
                if (scope is Scope.Host && scope.host != host.hostId) continue
                val keys = on(host.hostId)
                add(
                    group(
                        title = host.hostName,
                        description = "${hostState(host, now)} · ${sessions(keys.size)} · on ${machine.name}",
                        rows = forest(keys, Grouping.Machines),
                    ),
                )
            }
        }
    }

    private fun group(title: String, description: String, rows: List<SessionRow>) = Group(
        title = title,
        description = description,
        status = rows.map { it.attention }.maxByOrNull { it.priority() },
        rows = rows,
    )

    /**
     * The rows of [keys], given oldest first: newest first, each followed by its children among
     * [keys], oldest first.
     */
    private fun forest(keys: List<SessionKey>, grouping: Grouping): List<SessionRow> {
        val listed = keys.toSet()
        val children = mutableMapOf<SessionKey, MutableList<SessionKey>>()
        val roots = mutableListOf<SessionKey>()
        for (key in keys) {
            val parent = head(key)?.second?.parent
                ?.let { SessionKey(key.hostId, it) }
                ?.takeIf { it in listed && it != key }
            if (parent != null) children.getOrPut(parent) { mutableListOf() } += key else roots += key
        }
        val rows = mutableListOf<SessionRow>()
        val stack = ArrayDeque(roots.map { it to 0 })
        val seen = mutableSetOf<SessionKey>()
        while (stack.isNotEmpty()) {
            val (key, depth) = stack.removeLast()
            if (!seen.add(key)) continue
            row(key, depth, grouping)?.let(rows::add)
            children[key]?.asReversed()?.forEach { stack.addLast(it to depth + 1) }
        }
        return rows
    }

    private fun row(key: SessionKey, depth: Int, grouping: Grouping): SessionRow? {
        val (machine, head) = head(key) ?: return null
        val summary = summaries[key]
        val children = machine.sessions.count { it.sessionId != key.sessionId && it.parent == key.sessionId }
        return SessionRow(
            key = key,
            depth = depth,
            title = title(head, summary, grouping),
            status = head.status,
            children = children,
            needYou = if (children > 0) head.childrenNeedYou.toInt() else 0,
            prs = summary?.prs?.let(::ordered).orEmpty(),
            place = if (grouping == Grouping.Projects) place(machine, head) else null,
            movedTo = movedTo(key, head),
        )
    }

    /**
     * A child's task; else, under a project's heading that names the repo already, its branch,
     * and under a machine's, its repo's name and branch.
     */
    private fun title(head: SessionHead, summary: Summary?, grouping: Grouping): String {
        head.task?.let { return it }
        if (summary == null || !summary.loaded) return head.sessionId
        val branch = if (compact) summary.branch.substringAfterLast('/') else summary.branch
        val repo = summary.repo.split('/').lastOrNull { it.isNotEmpty() }
        return if (grouping == Grouping.Machines && repo != null) "$repo · $branch" else branch
    }

    /** For a `moved` copy, the host or machine whose copy of the session is not moved. */
    private fun movedTo(key: SessionKey, head: SessionHead): String? {
        if (head.status != SessionStatus.MOVED) return null
        return machines.firstNotNullOfOrNull { machine ->
            val copy = machine.sessions.find {
                it.sessionId == key.sessionId && it.status != SessionStatus.MOVED && machine.hostId != key.hostId
            } ?: return@firstNotNullOfOrNull null
            fleetHost(machine, copy)?.hostName ?: machine.name
        }
    }

    private fun head(key: SessionKey): Pair<Machine, SessionHead>? {
        val machine = machines.find { it.hostId == key.hostId } ?: return null
        val head = machine.sessions.find { it.sessionId == key.sessionId } ?: return null
        return machine to head
    }

    /** As its daemon lists it, else the local project of its repo; `null` until its repo is known. */
    private fun projectOf(machine: Machine, head: SessionHead): ProjectId? {
        head.projectId?.let { return it }
        val repo = summaries[key(machine, head)]?.repo
        // `ProjectId::local` in herder-protocol.
        return if (repo.isNullOrEmpty()) null else "${machine.hostId}:$repo"
    }

    /** As the first machine that lists it names it, else from its id. */
    private fun projectName(id: ProjectId): String =
        machines.flatMap { it.projects }.find { it.projectId == id }?.name ?: projectIdName(id)
}

/** Whether [scope] covers [machine]'s session [head]. */
private fun inScope(machine: Machine, head: SessionHead, scope: Scope): Boolean = when (scope) {
    Scope.All -> true
    is Scope.Machine -> machine.hostId == scope.hostId
    is Scope.Host -> machine.hostId == scope.vault && head.hostId == scope.host
}

/** The vault host a session runs on. */
private fun fleetHost(machine: Machine, head: SessionHead): FleetHost? =
    head.hostId?.let { host -> machine.hosts.find { it.hostId == host } }

/** Where a session runs: its machine; for a vault's, the host, marked when offline. */
private fun place(machine: Machine, head: SessionHead): String {
    val host = fleetHost(machine, head) ?: return machine.name
    return if (host.online) host.hostName else "${host.hostName} · offline"
}
