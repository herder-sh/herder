package sh.herder.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.herder.ffi.Answer
import sh.herder.ffi.Answerer
import sh.herder.ffi.ApprovalOutcome
import sh.herder.ffi.ErrorClass
import sh.herder.ffi.EscalationReason
import sh.herder.ffi.EventBody
import sh.herder.ffi.ItemBody
import sh.herder.ffi.PermissionMode
import sh.herder.ffi.Route
import sh.herder.ffi.SessionUpdate
import sh.herder.ffi.TurnError

// The GTK app's fold tests (linux/src/session.rs), with the same expectations.
class SessionTest {
    @Test
    fun aTurnWithAnApprovalFoldsIntoTheTranscript() {
        var session = Session().applied(
            updateOf(
                1, 0,
                sampleCreated(),
                EventBody.TurnStarted("t1"),
                added("i1", "t1", ItemBody.UserMessage("Run the tests.", emptyList())),
                toolCall("i2", "t1", "Bash", """{"command":"cargo test"}"""),
                EventBody.ApprovalRequested("a1", "t1", "i2", "Run cargo test", Route.USER, null),
            ),
        )
        assertEquals("app · herder/api", session.title)
        assertTrue(session.running)
        assertEquals(1, session.approvals.size)
        assertEquals(ToolApproval.Pending, session.toolApprovals["i2"])

        session = session.applied(
            updateOf(
                10, 70,
                EventBody.ApprovalResolved("a1", ApprovalOutcome.ALLOW, Answerer.User),
                toolResult("i3", "t1", "i2", "ok"),
                EventBody.TurnCompleted("t1"),
            ),
        )
        assertFalse(session.running)
        assertTrue(session.approvals.isEmpty())
        assertEquals("ok", session.result("i2")?.output)
        assertEquals(ToolApproval.Allowed, session.toolApprovals["i2"])
        val tail = session.entries.takeLast(3)
        assertEquals(Entry.Resolved(approval = true, text = "allowed Bash · by you"), tail[0])
        // Started at second 1, completed at second 72.
        assertEquals(Entry.TurnEnded(took = 71, interrupted = false, account = "claude-main", model = "opus"), tail[2])
    }

    @Test
    fun switchesFailuresAndQuestionsReadAsTheTuiSaysThem() {
        var session = Session().applied(
            updateOf(
                1, 0,
                sampleCreated(),
                EventBody.TurnStarted("t1"),
                EventBody.QuestionAsked("q1", "t1", "Which heading level?", listOf("h2", "h1"), Route.PRIMARY, null),
                EventBody.QuestionEscalated("q1", EscalationReason.TIMEOUT, "ask them"),
            ),
        )
        assertEquals(Route.USER, session.questions[0].routedTo)
        assertEquals("ask them", session.questions[0].note)
        assertEquals(Entry.Notice("question for the primary session: Which heading level?"), session.entries[0])
        assertEquals(Entry.Notice("question escalated to you: the primary session did not answer in time", true), session.entries[1])

        session = session.applied(
            updateOf(
                10, 10,
                EventBody.TurnFailed("t1", TurnError(ErrorClass.LIMIT_REACHED, "5h limit")),
                // Nobody asked for this one: a failover.
                EventBody.AccountSwitched("claude-alt"),
                EventBody.ProviderSwitched("codex", "codex-work", "gpt-5"),
                EventBody.PermissionModeChanged(PermissionMode.AUTO_EDIT),
            ).let { update ->
                update.copy(events = update.events.mapIndexed { n, event -> if (n == 1) event.copy(by = null) else event })
            },
        )
        // The failed turn took its question with it.
        assertTrue(session.questions.isEmpty())
        assertEquals("codex", session.provider)
        assertEquals("gpt-5", session.model)
        assertEquals(PermissionMode.AUTO_EDIT, session.permissionMode)
        assertEquals(Entry.TurnFailed(ErrorClass.LIMIT_REACHED, "5h limit"), session.entries[2])
        assertEquals(
            listOf(
                "failed over to claude-alt: the last account hit its limit",
                "switched to codex-work · gpt-5 (transcript replayed)",
                "mode set to auto_edit",
            ),
            session.entries.filterIsInstance<Entry.Switch>().map { it.text },
        )
    }

    @Test
    fun anAnsweredQuestionNamesTheChoice() {
        val session = Session().applied(
            updateOf(
                1, 0,
                sampleCreated(),
                EventBody.TurnStarted("t1"),
                EventBody.QuestionAsked("q1", "t1", "Which heading level?\nPick one.", listOf("h2", "h1"), Route.USER, null),
                EventBody.QuestionAnswered("q1", Answer.Choice(1u), Answerer.User),
            ),
        )
        assertTrue(session.questions.isEmpty())
        assertEquals(Entry.Resolved(approval = false, text = "Which heading level? · h1 · by you"), session.entries.last())
    }

    @Test
    fun streamingItemsAreReplacedByEachUpdate() {
        val streaming = listOf(sh.herder.ffi.Item("i1", "t1", ItemBody.AssistantMessage("Hel")))
        var session = Session().applied(SessionUpdate(emptyList(), streaming))
        assertTrue(session.loaded)
        assertEquals(streaming, session.streaming)
        session = session.applied(SessionUpdate(emptyList(), emptyList()))
        assertTrue(session.streaming.isEmpty())
    }

    @Test
    fun durationsReadAsInTheTui() {
        assertEquals("4s", duration(4))
        assertEquals("1m 02s", duration(62))
        assertEquals("2h 05m", duration(7500))
    }

    @Test
    fun recentModelsAreTheProvidersOwn() {
        val summaries = mapOf(
            SessionKey("h1", "s1") to Summary().applied(update("s1", created("/srv/app", "a"))),
            SessionKey("h1", "s2") to Summary().applied(
                update("s2", created("/srv/app", "b"), EventBody.ModelSwitched("claude-sonnet")),
            ),
            SessionKey("h1", "s3") to Summary().applied(
                update("s3", created("/srv/app", "c"), EventBody.ProviderSwitched("codex", "codex-work", "gpt-5")),
            ),
        )
        assertEquals(listOf("claude-opus", "claude-sonnet"), recentModels(summaries, "claude"))
        assertEquals(listOf("gpt-5"), recentModels(summaries, "codex"))
    }
}
