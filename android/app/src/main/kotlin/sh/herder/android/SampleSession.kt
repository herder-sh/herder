package sh.herder.android

import java.time.Instant
import sh.herder.ffi.Account
import sh.herder.ffi.CiStatus
import sh.herder.ffi.ConnectionState
import sh.herder.ffi.Event
import sh.herder.ffi.EventBody
import sh.herder.ffi.Item
import sh.herder.ffi.ItemBody
import sh.herder.ffi.Machine
import sh.herder.ffi.PermissionMode
import sh.herder.ffi.PrState
import sh.herder.ffi.Route
import sh.herder.ffi.SessionStatus
import sh.herder.ffi.SessionUpdate
import sh.herder.ffi.UsageWindow

// A session mid-turn, for previews, tests and screenshots: a finished turn that read, searched,
// edited and tested, then a second turn at one of its [Stage]s.

/** When the sample's first event happened. */
internal val SampleStart: Instant = Instant.parse("2026-10-03T12:00:00Z")

/** The sample session, on `box`. */
internal val SampleKey = SessionKey("h1", "s2")

/** Where the sample's second turn is. */
internal enum class Stage {
    /** Running, its reply streaming. */
    Chat,

    /** Waiting for an approval to remove the build output. */
    Approval,

    /** Waiting for an answer to a question. */
    Question,

    /** Done, after switching provider and mode and linking a PR. */
    Switched,
}

/** An event of the sample session, [second]s after [SampleStart]. */
internal fun event(seq: Int, second: Long, body: EventBody, by: String? = null) = Event(
    sessionId = SampleKey.sessionId,
    seq = seq.toULong(),
    at = SampleStart.plusSeconds(second).toString(),
    by = by,
    body = body,
)

/** An update of [bodies], one second apart from [second] on, numbered from [seq]. */
internal fun updateOf(seq: Int, second: Long, vararg bodies: EventBody, streaming: List<Item> = emptyList()) =
    SessionUpdate(
        events = bodies.mapIndexed { n, body -> event(seq + n, second + n, body, by = "you".takeIf { body.byUser() }) },
        streaming = streaming,
    )

/** A user's own doing, as against the daemon's or the agent's. */
private fun EventBody.byUser(): Boolean = this is EventBody.ModelSwitched || this is EventBody.AccountSwitched ||
    this is EventBody.ProviderSwitched || this is EventBody.PermissionModeChanged

internal fun added(id: String, turn: String, body: ItemBody): EventBody = EventBody.ItemAdded(Item(id, turn, body))

internal fun toolCall(id: String, turn: String, name: String, input: String) = added(id, turn, ItemBody.ToolCall(name, input))

internal fun toolResult(id: String, turn: String, call: String, output: String, error: Boolean = false) =
    added(id, turn, ItemBody.ToolResult(call, output, error))

/** The sample's first event. */
internal fun sampleCreated(): EventBody = created("/srv/app", "herder/api").let {
    (it as EventBody.SessionCreated).copy(worktree = "/srv/wt/api", model = "opus")
}

/** The sample's first turn, finished: read, search, edit, test and a reply. */
internal fun firstTurn(): SessionUpdate = updateOf(
    1, 0,
    sampleCreated(),
    EventBody.SessionStatusChanged(SessionStatus.RUNNING),
    EventBody.TurnStarted("t1"),
    added("i1", "t1", ItemBody.UserMessage("Add a health endpoint and test it.", emptyList())),
    added("i2", "t1", ItemBody.Reasoning("Where the router lives\nThe routes are built in src/api.rs.")),
    toolCall("i3", "t1", "Read", """{"file_path":"/srv/wt/api/src/api.rs"}"""),
    toolResult("i4", "t1", "i3", "pub fn router() -> Router {\n    Router::new().route(\"/\", get(index))\n}"),
    toolCall("i5", "t1", "Grep", """{"pattern":"Router::new","path":"/srv/wt/api/src"}"""),
    toolResult("i6", "t1", "i5", "Found 3 files\nsrc/api.rs\nsrc/main.rs\nsrc/tests.rs"),
    toolCall(
        "i7", "t1", "Edit",
        """{"file_path":"/srv/wt/api/src/api.rs","old_string":"    Router::new().route(\"/\", get(index))","new_string":"    Router::new()\n        .route(\"/\", get(index))\n        .route(\"/health\", get(health))"}""",
    ),
    toolResult("i8", "t1", "i7", "The file has been updated."),
    toolCall("i9", "t1", "Bash", """{"command":"cargo test --workspace"}"""),
    toolResult(
        "i10", "t1", "i9",
        "running 12 tests\ntest health::ok ... ok\ntest index::ok ... ok\n\ntest result: ok. 12 passed; 0 failed",
    ),
    added(
        "i11", "t1",
        ItemBody.AssistantMessage(
            "I added `GET /health`; it returns **200** with the build version so load balancers can probe it.\n\n" +
                "- the route is in `src/api.rs`\n- `cargo test` passes",
        ),
    ),
    EventBody.TurnCompleted("t1"),
    EventBody.SessionStatusChanged(SessionStatus.IDLE),
)

/** The sample session with its second turn at [stage]. */
internal fun sampleSession(stage: Stage): Session {
    var session = Session().applied(firstTurn())
    val second = updateOf(
        100, 60,
        EventBody.SessionStatusChanged(SessionStatus.RUNNING),
        EventBody.TurnStarted("t2"),
        added("j1", "t2", ItemBody.UserMessage("Remove the old build output too.", emptyList())),
        toolCall("j2", "t2", "Bash", """{"command":"rm -rf /srv/wt/api/target/"}"""),
    )
    session = session.applied(second)
    return when (stage) {
        Stage.Chat -> session.applied(
            SessionUpdate(
                events = emptyList(),
                streaming = listOf(Item("j3", "t2", ItemBody.AssistantMessage("Removing the old `target/` directory, then I'll"))),
            ),
        )
        Stage.Approval -> session.applied(
            updateOf(
                200, 66,
                EventBody.ApprovalRequested("a1", "t2", "j2", "Remove the build output", Route.USER, null),
            ),
        )
        Stage.Question -> session.applied(
            updateOf(
                200, 66,
                EventBody.QuestionAsked(
                    "q1", "t2", "Which heading level for the API page?",
                    listOf("h2 under Reference", "h1, its own page"), Route.USER, null,
                ),
            ),
        )
        Stage.Switched -> session.applied(
            updateOf(
                200, 66,
                EventBody.ApprovalRequested("a1", "t2", "j2", "Remove the build output", Route.USER, null),
                EventBody.ApprovalResolved("a1", sh.herder.ffi.ApprovalOutcome.DENY, sh.herder.ffi.Answerer.User),
                added("j4", "t2", ItemBody.AssistantMessage("Left `target/` in place.")),
                EventBody.TurnCompleted("t2"),
                EventBody.SessionStatusChanged(SessionStatus.IDLE),
                EventBody.ProviderSwitched("codex", "codex-work", "gpt-5"),
                EventBody.PermissionModeChanged(PermissionMode.AUTO_EDIT),
                EventBody.PrLinked(pr(12, PrState.OPEN, CiStatus.PASSING).copy(title = "Add a health endpoint")),
            ),
        )
    }
}

/** An account, its busiest window [percent] used. */
internal fun account(id: String, provider: String, label: String, window: String, percent: Double) =
    Account(id, provider, label, listOf(UsageWindow(window, percent, null)))

/** `box`, with the sample session and three accounts of two providers. */
internal fun sampleMachine(status: SessionStatus = SessionStatus.RUNNING): Machine = machine(
    "h1", "box", ConnectionState.Connected,
    listOf(head("s2", "github.com/org/app").copy(status = status)),
).copy(
    accounts = listOf(
        account("claude-main", "claude", "Claude (work)", "five_hour", 38.0),
        account("claude-alt", "claude", "Claude (personal)", "five_hour", 4.0),
        account("codex-work", "codex", "Codex (work)", "daily", 91.0),
    ),
)
