package sh.herder.android

import java.time.OffsetDateTime
import java.time.format.DateTimeParseException
import sh.herder.ffi.AccountId
import sh.herder.ffi.Answer
import sh.herder.ffi.Answerer
import sh.herder.ffi.ApprovalId
import sh.herder.ffi.ApprovalOutcome
import sh.herder.ffi.ErrorClass
import sh.herder.ffi.EscalationReason
import sh.herder.ffi.Event
import sh.herder.ffi.EventBody
import sh.herder.ffi.HerderException
import sh.herder.ffi.Item
import sh.herder.ffi.ItemBody
import sh.herder.ffi.ItemId
import sh.herder.ffi.PermissionMode
import sh.herder.ffi.PullRequest
import sh.herder.ffi.QuestionId
import sh.herder.ffi.Route
import sh.herder.ffi.SessionId
import sh.herder.ffi.SessionStatus
import sh.herder.ffi.SessionUpdate
import sh.herder.ffi.TurnId

// What the app knows of one session, folded from its subscription's updates: the transcript
// the session view draws, the requests waiting on someone, and what the composer's controls
// show. The same fold as the GTK app's `linux/src/session.rs` and the TUI's, in the same words.

/** One session as built from its events. */
data class Session(
    /** Whether its first update arrived; until then nothing below is known. */
    val loaded: Boolean = false,
    /** Repository path on the host. */
    val repo: String = "",
    /** The session's worktree on the host. */
    val worktree: String = "",
    /** Branch the session works on now. */
    val branch: String = "",
    /** Task label, for a child session. */
    val task: String? = null,
    /** Current model, in the provider's naming. */
    val model: String = "",
    /** Account it runs on. */
    val accountId: AccountId? = null,
    /** Provider of that account. */
    val provider: String? = null,
    val permissionMode: PermissionMode = PermissionMode.ASK,
    /** Latest status, as its events say. */
    val status: SessionStatus = SessionStatus.IDLE,
    /** Completed transcript entries, oldest first. Only ever extended. */
    val entries: List<Entry> = emptyList(),
    /** Items streaming now, with their text so far. */
    val streaming: List<Item> = emptyList(),
    /** When the running turn started, in epoch seconds; `null` between turns or when unknown. */
    val turnStarted: Long? = null,
    /** The running turn; `null` between turns. */
    val turn: TurnId? = null,
    /** Approval requests nobody answered yet, oldest first. */
    val approvals: List<PendingApproval> = emptyList(),
    /** Questions nobody answered yet, oldest first. */
    val questions: List<PendingQuestion> = emptyList(),
    /** How the approval asked for each tool call stands. */
    val toolApprovals: Map<ItemId, ToolApproval> = emptyMap(),
    /** Pull requests linked to the session now, in the order they were linked. */
    val prs: List<PullRequest> = emptyList(),
) {
    /** The task label, else the repo's name and branch, as the TUI titles a session. */
    val title: String
        get() {
            task?.let { return it }
            val repo = repo.split('/').lastOrNull { it.isNotEmpty() } ?: return branch
            return "$repo · $branch"
        }

    /** Whether a turn runs now. */
    val running: Boolean get() = turn != null

    /** The result of the tool call [id]. */
    fun result(id: ItemId): ItemBody.ToolResult? = entries.firstNotNullOfOrNull { entry ->
        ((entry as? Entry.Added)?.item?.body as? ItemBody.ToolResult)?.takeIf { it.callId == id }
    }

    /** The tool call item [id]. */
    fun toolCall(id: ItemId): ItemBody.ToolCall? = entries.firstNotNullOfOrNull { entry ->
        val item = (entry as? Entry.Added)?.item ?: return@firstNotNullOfOrNull null
        (item.body as? ItemBody.ToolCall)?.takeIf { item.id == id }
    }

    /** This session with [update] folded in. */
    fun applied(update: SessionUpdate): Session {
        val fold = Fold(this)
        update.events.forEach(fold::event)
        return fold.session.copy(loaded = true, entries = fold.entries.toList(), streaming = update.streaming)
    }
}

/** An approval request waiting for an answer. */
data class PendingApproval(
    val id: ApprovalId,
    /** The tool call it asks about. */
    val toolCallId: ItemId,
    /** What the agent wants to do. */
    val summary: String,
    /** Who is asked first; a user can always answer. */
    val routedTo: Route,
    /** Why a child's request went to the user rather than its primary session. */
    val reason: EscalationReason?,
    /** What the primary session said when it escalated the request. */
    val note: String?,
    /** When it was put to whoever it waits on now, in epoch seconds. */
    val since: Long?,
)

/** A question waiting for an answer. */
data class PendingQuestion(
    val id: QuestionId,
    val turnId: TurnId,
    val text: String,
    /** Answers to pick from; empty for free text. */
    val choices: List<String>,
    val routedTo: Route,
    val reason: EscalationReason?,
    val note: String?,
    val since: Long?,
)

/** Where an approval for a tool call stands. */
enum class ToolApproval {
    Pending,
    Allowed,

    /** Denied, or expired: the tool did not run. */
    Denied,
}

/** One completed entry of the transcript. */
sealed interface Entry {
    /** A transcript item, complete. */
    data class Added(val item: Item) : Entry

    /** Something the session went through that is not an item, as one line. */
    data class Notice(val text: String, val attention: Boolean = false) : Entry

    /** A turn ended without failing: the footer of the agent's reply. */
    data class TurnEnded(val took: Long?, val interrupted: Boolean, val account: String, val model: String) : Entry

    data class TurnFailed(val errorClass: ErrorClass, val message: String) : Entry

    /** The session moved to another model, account, provider or permission mode. */
    data class Switch(val text: String) : Entry

    /** An approval or question was answered, or expired. */
    data class Resolved(val approval: Boolean, val text: String) : Entry

    /** The session spawned a child for a task. */
    data class Child(val sessionId: SessionId, val task: String) : Entry

    /** A child finished a turn and reported back. */
    data class Report(val summary: String) : Entry

    /** A pull request was linked; drawn as it stands now, from [Session.prs]. */
    data class Pr(val number: ULong) : Entry
}

/** The fold of one update's events, from [session] on; [entries] are the session's from then on. */
private class Fold(var session: Session) {
    val entries = session.entries.toMutableList()

    fun event(event: Event) {
        val at = seconds(event.at)
        val s = session
        val entry: Entry? = when (val body = event.body) {
            is EventBody.SessionCreated -> {
                session = s.copy(
                    repo = body.repo,
                    worktree = body.worktree,
                    branch = body.branch,
                    provider = body.provider,
                    accountId = body.accountId,
                    model = body.model,
                    permissionMode = body.permissionMode,
                    task = body.task,
                )
                null
            }
            is EventBody.SessionStatusChanged -> {
                session = s.copy(status = body.status)
                null
            }
            is EventBody.BranchCheckedOut -> {
                session = s.copy(branch = body.branch)
                Entry.Notice("checked out ${body.branch}")
            }
            is EventBody.ItemAdded -> Entry.Added(body.item)
            is EventBody.TurnStarted -> {
                session = s.copy(turn = body.turnId, turnStarted = at)
                null
            }
            is EventBody.TurnCompleted -> turnEnded(body.turnId, at, interrupted = false)
            is EventBody.TurnInterrupted -> turnEnded(body.turnId, at, interrupted = true)
            is EventBody.TurnFailed -> {
                endTurn(body.turnId, at)
                Entry.TurnFailed(body.error.`class`, body.error.message)
            }
            is EventBody.ApprovalRequested -> {
                session = s.copy(
                    toolApprovals = s.toolApprovals + (body.toolCallId to ToolApproval.Pending),
                    approvals = s.approvals + PendingApproval(
                        id = body.approvalId,
                        toolCallId = body.toolCallId,
                        summary = body.summary,
                        routedTo = body.routedTo,
                        reason = body.reason,
                        note = null,
                        since = at,
                    ),
                )
                asked("approval", body.summary, body.routedTo)
            }
            is EventBody.ApprovalEscalated -> {
                session = s.copy(
                    approvals = s.approvals.map {
                        if (it.id != body.approvalId) it
                        else it.copy(routedTo = Route.USER, reason = body.reason, note = body.note, since = at)
                    },
                )
                escalated("approval", body.reason)
            }
            is EventBody.ApprovalResolved -> {
                val resolved = s.approvals.find { it.id == body.approvalId }
                var toolApprovals = s.toolApprovals
                val tool = resolved?.let { approval ->
                    val state = if (body.decision == ApprovalOutcome.ALLOW) ToolApproval.Allowed else ToolApproval.Denied
                    toolApprovals = toolApprovals + (approval.toolCallId to state)
                    toolCall(approval.toolCallId)?.name ?: approval.summary
                }
                session = s.copy(approvals = s.approvals - setOfNotNull(resolved), toolApprovals = toolApprovals)
                val decision = when (body.decision) {
                    ApprovalOutcome.ALLOW -> "allowed"
                    ApprovalOutcome.DENY -> "denied"
                    ApprovalOutcome.EXPIRED -> "expired"
                }
                val what = if (tool == null) decision else "$decision $tool"
                Entry.Resolved(approval = true, text = "$what · by ${answerer(body.answeredBy)}")
            }
            is EventBody.QuestionAsked -> {
                session = s.copy(
                    questions = s.questions + PendingQuestion(
                        id = body.questionId,
                        turnId = body.turnId,
                        text = body.text,
                        choices = body.choices,
                        routedTo = body.routedTo,
                        reason = body.reason,
                        note = null,
                        since = at,
                    ),
                )
                asked("question", body.text, body.routedTo)
            }
            is EventBody.QuestionEscalated -> {
                session = s.copy(
                    questions = s.questions.map {
                        if (it.id != body.questionId) it
                        else it.copy(routedTo = Route.USER, reason = body.reason, note = body.note, since = at)
                    },
                )
                escalated("question", body.reason)
            }
            is EventBody.QuestionAnswered -> {
                val asked = s.questions.find { it.id == body.questionId }
                session = s.copy(questions = s.questions - setOfNotNull(asked))
                val answer = when (val answer = body.answer) {
                    is Answer.Text -> answer.text
                    is Answer.Choice -> asked?.choices?.getOrNull(answer.index.toInt())
                        ?: "choice ${answer.index + 1u}"
                }
                val by = answerer(body.answeredBy)
                Entry.Resolved(
                    approval = false,
                    text = if (asked != null) "${firstLine(asked.text)} · $answer · by $by" else "answered $answer · by $by",
                )
            }
            is EventBody.ChildSpawned -> Entry.Child(body.childSessionId, body.task)
            is EventBody.ChildReported -> Entry.Report(body.summary)
            is EventBody.ModelSwitched -> {
                session = s.copy(model = body.model)
                Entry.Switch("switched to ${body.model}")
            }
            // A switch nobody asked for is a failover from an account that hit its limit.
            is EventBody.AccountSwitched -> {
                session = s.copy(accountId = body.accountId)
                Entry.Switch(
                    if (event.by != null) "switched to ${body.accountId}"
                    else "failed over to ${body.accountId}: the last account hit its limit",
                )
            }
            is EventBody.ProviderSwitched -> {
                session = s.copy(provider = body.provider, accountId = body.accountId, model = body.model)
                val to = "${body.accountId} · ${body.model} (transcript replayed)"
                Entry.Switch(if (event.by != null) "switched to $to" else "failed over to $to")
            }
            is EventBody.PermissionModeChanged -> {
                session = s.copy(permissionMode = body.mode)
                Entry.Switch("mode set to ${body.mode.label()}")
            }
            is EventBody.PrLinked -> {
                track(body.pr)
                Entry.Pr(body.pr.number)
            }
            is EventBody.PrUpdated -> {
                track(body.pr)
                null
            }
            is EventBody.PrUnlinked -> {
                session = s.copy(prs = s.prs.filter { it.number != body.number })
                Entry.Notice("pull request #${body.number} unlinked")
            }
            EventBody.Unknown -> null
        }
        entry?.let(entries::add)
    }

    private fun toolCall(id: ItemId): ItemBody.ToolCall? = entries.firstNotNullOfOrNull { entry ->
        val item = (entry as? Entry.Added)?.item ?: return@firstNotNullOfOrNull null
        (item.body as? ItemBody.ToolCall)?.takeIf { item.id == id }
    }

    private fun turnEnded(turnId: TurnId, at: Long?, interrupted: Boolean): Entry {
        val took = endTurn(turnId, at)
        return Entry.TurnEnded(took, interrupted, session.accountId.orEmpty(), session.model)
    }

    /** Ends [turnId] at [at]; how many seconds it took, when its start is known. */
    private fun endTurn(turnId: TurnId, at: Long?): Long? {
        var s = session
        var took: Long? = null
        if (s.turn == turnId) {
            took = s.turnStarted?.let { start -> at?.let { it - start } }
            s = s.copy(turn = null, turnStarted = null)
        }
        // A question dies with its turn; approvals are resolved by the daemon.
        session = s.copy(questions = s.questions.filter { it.turnId != turnId })
        return took
    }

    private fun track(pr: PullRequest) {
        val prs = session.prs.toMutableList()
        val known = prs.indexOfFirst { it.number == pr.number }
        if (known >= 0) prs[known] = pr else prs += pr
        session = session.copy(prs = prs)
    }
}

/**
 * The line for a request put to the primary session first. One put to the user has none: its
 * card shows it until it is answered.
 */
private fun asked(what: String, text: String, routedTo: Route): Entry? =
    if (routedTo == Route.PRIMARY) Entry.Notice("$what for the primary session: ${firstLine(text)}") else null

private fun escalated(what: String, reason: EscalationReason) =
    Entry.Notice("$what escalated to you: ${reason.text()}", attention = true)

private fun answerer(answerer: Answerer): String = when (answerer) {
    is Answerer.Primary -> "the primary session"
    Answerer.User -> "you"
}

/** An RFC 3339 timestamp in epoch seconds; `null` when it does not parse. */
fun seconds(at: String): Long? = try {
    OffsetDateTime.parse(at).toEpochSecond()
} catch (_: DateTimeParseException) {
    null
}

/** The first non-blank line of [text]. */
fun firstLine(text: String): String = text.lineSequence().map { it.trim() }.firstOrNull { it.isNotEmpty() }.orEmpty()

/** Why a child's request is the user's, in words. */
fun EscalationReason.text(): String = when (this) {
    EscalationReason.MARKED_BY_PRIMARY -> "the primary session left it to you"
    EscalationReason.EXCEEDS_AUTHORITY -> "beyond what the primary session may decide"
    EscalationReason.TIMEOUT -> "the primary session did not answer in time"
}

/** A permission mode as the TUI names it. */
fun PermissionMode.label(): String = when (this) {
    PermissionMode.READ_ONLY -> "read_only"
    PermissionMode.ASK -> "ask"
    PermissionMode.AUTO_EDIT -> "auto_edit"
    PermissionMode.FULL_ACCESS -> "full_access"
}

/** What a permission mode lets the agent do, in words. */
fun PermissionMode.description(): String = when (this) {
    PermissionMode.READ_ONLY -> "Reads only; every write or command is refused"
    PermissionMode.ASK -> "Asks before every write or command"
    PermissionMode.AUTO_EDIT -> "Edits files freely; asks before commands"
    PermissionMode.FULL_ACCESS -> "Does anything without asking"
}

/** A failure's class in words, as the TUI's failure card heads it. */
fun ErrorClass.label(): String = when (this) {
    ErrorClass.LIMIT_REACHED -> "limit reached"
    ErrorClass.AUTH -> "not signed in"
    ErrorClass.TRANSIENT -> "transient error"
    ErrorClass.FATAL -> "failed"
}

/** Seconds as the TUI shows a duration: `42s`, `1m 02s`, `2h 05m`. */
fun duration(seconds: Long): String {
    val s = seconds.coerceAtLeast(0)
    return when {
        s < 60 -> "${s}s"
        s < 3600 -> "${s / 60}m ${"%02d".format(s % 60)}s"
        else -> "${s / 3600}h ${"%02d".format(s % 3600 / 60)}m"
    }
}

/** A limit window's short name, as the TUI's. */
fun windowLabel(window: String): String = when (window) {
    "five_hour" -> "5h"
    "seven_day", "weekly" -> "week"
    "daily" -> "day"
    else -> window.replace('_', ' ')
}

/** Why a command failed, for a snackbar. */
fun HerderException.reason(): String = when (this) {
    is HerderException.Rejected -> info.message
    is HerderException.InvalidLink -> detail
    is HerderException.Pairing -> "pairing failed: $detail"
    is HerderException.UnknownMachine -> "no paired machine $hostId"
    is HerderException.Local -> detail
    is HerderException.Closed -> "the client stopped"
}
