package sh.herder.android.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTextReplacement
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.swipeLeft
import androidx.compose.ui.test.swipeRight
import java.time.Instant
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import sh.herder.android.Profile
import sh.herder.android.SampleKey
import sh.herder.android.SampleStart
import sh.herder.android.Session
import sh.herder.android.Stage
import sh.herder.android.Summary
import sh.herder.android.added
import sh.herder.android.firstTurn
import sh.herder.android.sampleMachine
import sh.herder.android.sampleSession
import sh.herder.android.toolCall
import sh.herder.android.toolResult
import sh.herder.android.updateOf
import sh.herder.ffi.Answer
import sh.herder.ffi.Answerer
import sh.herder.ffi.ApprovalDecision
import sh.herder.ffi.ApprovalOutcome
import sh.herder.ffi.CommandBody
import sh.herder.ffi.EventBody
import sh.herder.ffi.Item
import sh.herder.ffi.ItemBody
import sh.herder.ffi.Machine
import sh.herder.ffi.PermissionMode
import sh.herder.ffi.Route
import sh.herder.ffi.SessionStatus
import sh.herder.ffi.SessionUpdate

/** The commands the screen sent, each answered with [refusal]: `null` applies it. */
private class Recorder(var refusal: String? = null) {
    val sent = mutableListOf<CommandBody>()
    val send: Sender = { _, command ->
        sent += command
        refusal
    }
}

// A phone, as the session view is driven from one.
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w411dp-h891dp")
class SessionScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val clock = { SampleStart.plusSeconds(90) }
    private val s2 = SampleKey.sessionId

    private fun show(session: Session, machine: Machine = sampleMachine(SessionStatus.IDLE), recorder: Recorder = Recorder()): Recorder {
        compose.setContent {
            HerderTheme {
                SessionScreen(SampleKey, session, machine, listOf("opus", "sonnet"), recorder.send, {}, {}, clock)
            }
        }
        return recorder
    }

    private fun composer() = compose.onNode(hasSetTextAction())

    @Test
    fun aFullTurnFromAPhone() {
        val recorder = Recorder()
        var session by mutableStateOf(Session().applied(firstTurn()))
        val profile = Profile.Open(
            listOf(sampleMachine(SessionStatus.IDLE)),
            mapOf(SampleKey to Summary().applied(firstTurn())),
        )
        compose.setContent {
            HerderTheme {
                MachinesScreen(profile, SampleStart) { key, _, _, onBack ->
                    SessionScreen(key, session, profile.machines[0], listOf("opus", "sonnet"), recorder.send, {}, onBack, clock)
                }
            }
        }
        // Machines, then the machine's sessions, then the session.
        compose.onNodeWithText("box").performClick()
        compose.onNodeWithText("api").performClick()
        compose.onNodeWithText("app · herder/api").assertIsDisplayed()

        // A prompt waits, queued, until it joins the transcript.
        composer().performTextInput("Remove the old build output too.")
        compose.onNodeWithContentDescription("Send").performClick()
        assertEquals(CommandBody.SendPrompt(s2, "Remove the old build output too.", emptyList()), recorder.sent.last())
        compose.onNodeWithText("QUEUED").assertIsDisplayed()
        session = session.applied(
            updateOf(
                100, 60,
                EventBody.SessionStatusChanged(SessionStatus.RUNNING, null),
                EventBody.TurnStarted("t2"),
                added("j1", "t2", ItemBody.UserMessage("Remove the old build output too.", emptyList())),
                streaming = listOf(Item(null, "j2", "t2", ItemBody.AssistantMessage("Removing it"))),
            ),
        )
        compose.onAllNodesWithText("QUEUED").assertCountEquals(0)
        // The reply streams with a cursor; the turn can be stopped.
        compose.onNodeWithText("Removing it ▌").assertIsDisplayed()
        compose.onNodeWithContentDescription("Stop").assertIsDisplayed()

        // The tool call asks; the card replaces the composer.
        session = session.applied(
            updateOf(
                110, 63,
                toolCall("j3", "t2", "Bash", """{"command":"rm -rf /srv/wt/api/target/"}"""),
                EventBody.ApprovalRequested("a1", "t2", "j3", "Remove the build output", Route.USER, null),
            ),
        )
        compose.onNodeWithText("$ rm -rf target/").assertIsDisplayed()
        compose.onAllNodes(hasContentDescription("Send")).assertCountEquals(0)
        compose.onNodeWithText("Allow").performClick()
        assertEquals(CommandBody.AnswerApproval(s2, "a1", ApprovalDecision.ALLOW), recorder.sent.last())

        // Allowed, the tool runs and the turn ends with its footer.
        session = session.applied(
            updateOf(
                120, 90,
                EventBody.ApprovalResolved("a1", ApprovalOutcome.ALLOW, Answerer.User),
                toolResult("j4", "t2", "j3", "removed 1 directory"),
                added("j5", "t2", ItemBody.AssistantMessage("Removed `target/`.")),
                EventBody.TurnCompleted("t2"),
                EventBody.SessionStatusChanged(SessionStatus.IDLE, null),
            ),
        )
        compose.onNodeWithText("△ allowed Bash · by you").assertIsDisplayed()
        compose.onNodeWithText("▣ claude-main · opus · 32s").assertIsDisplayed()
        compose.onNodeWithContentDescription("Send").assertIsDisplayed()

        // Then the model switches from the composer.
        compose.onNodeWithContentDescription("Model: opus").performClick()
        // The model replaces the prompt while it is picked.
        compose.onNode(hasSetTextAction()).performTextReplacement("sonnet")
        compose.onNodeWithText("Switch").performClick()
        assertEquals(CommandBody.SetModel(s2, "sonnet"), recorder.sent.last())
        session = session.applied(updateOf(130, 80, EventBody.ModelSwitched("sonnet")))
        compose.onNodeWithText("switched to sonnet").assertIsDisplayed()
        compose.onNodeWithContentDescription("Model: sonnet").assertIsDisplayed()
    }

    @Test
    fun toolCallsAreOneRowThatExpandsOnTap() {
        show(Session().applied(firstTurn()))
        compose.onNodeWithText("cargo test --workspace").assertIsDisplayed()
        compose.onAllNodesWithText("running 12 tests").assertCountEquals(0)
        compose.onNodeWithText("cargo test --workspace").performClick()
        compose.onNodeWithText("running 12 tests").assertIsDisplayed()
        compose.onNodeWithText("$ cargo test --workspace").assertIsDisplayed()
        // An edit counts its lines and shows its diff.
        compose.onNodeWithText("Edit src/api.rs  +3 −1").performClick()
        compose.onNodeWithText("+ " + " ".repeat(8) + ".route(\"/health\", get(health))").assertIsDisplayed()
    }

    @Test
    fun anApprovalIsAnsweredBySwiping() {
        val recorder = show(sampleSession(Stage.Approval))
        compose.onNodeWithText("Swipe right to allow, left to deny").performTouchInput { swipeLeft() }
        assertEquals(listOf(CommandBody.AnswerApproval(s2, "a1", ApprovalDecision.DENY)), recorder.sent)
    }

    @Test
    fun aSwipeRightAllows() {
        val recorder = show(sampleSession(Stage.Approval))
        compose.onNodeWithText("asked 24s ago").assertIsDisplayed()
        compose.onNodeWithText("Swipe right to allow, left to deny").performTouchInput { swipeRight() }
        assertEquals(listOf(CommandBody.AnswerApproval(s2, "a1", ApprovalDecision.ALLOW)), recorder.sent)
    }

    @Test
    fun aQuestionIsAnsweredWithAChoiceOrText() {
        val recorder = show(sampleSession(Stage.Question))
        compose.onNodeWithText("Which heading level for the API page?").assertIsDisplayed()
        compose.onNodeWithText("h1, its own page").performClick()
        assertEquals(CommandBody.AnswerQuestion(s2, "q1", Answer.Choice(1u)), recorder.sent.last())
    }

    @Test
    fun aQuestionTakesATypedAnswer() {
        val recorder = show(sampleSession(Stage.Question))
        compose.onNode(hasSetTextAction()).performTextInput("h3")
        compose.onNodeWithContentDescription("Answer").performClick()
        assertEquals(CommandBody.AnswerQuestion(s2, "q1", Answer.Text("h3")), recorder.sent.last())
    }

    @Test
    fun theAccountPickerSwitchesAccountOrProvider() {
        val recorder = show(sampleSession(Stage.Chat).copy(turn = null))
        compose.onNodeWithContentDescription("Account: claude-main").performClick()
        compose.onNodeWithText("Same provider · the conversation continues").assertIsDisplayed()
        compose.onNodeWithText("Other provider · replays the transcript").assertIsDisplayed()
        compose.onNodeWithText("day 91%").assertIsDisplayed()
        compose.onNodeWithText("Claude (personal)").performClick()
        assertEquals(CommandBody.SwitchAccount(s2, "claude-alt"), recorder.sent.last())

        compose.onNodeWithContentDescription("Account: claude-main").performClick()
        compose.onNodeWithText("Codex (work)").performClick()
        assertEquals(CommandBody.SwitchProvider(s2, "codex-work", null), recorder.sent.last())
    }

    @Test
    fun theModePickerSetsTheMode() {
        val recorder = show(sampleSession(Stage.Switched))
        compose.onNodeWithContentDescription("Mode: auto_edit").performClick()
        compose.onNodeWithText("Asks before every write or command").assertIsDisplayed()
        compose.onNodeWithText("read_only").performClick()
        assertEquals(CommandBody.SetPermissionMode(s2, PermissionMode.READ_ONLY), recorder.sent.last())
    }

    @Test
    fun stopInterruptsTheTurn() {
        val recorder = show(sampleSession(Stage.Chat), sampleMachine(SessionStatus.RUNNING))
        compose.onNodeWithText("working · 29s").assertIsDisplayed()
        compose.onNodeWithText("Queue a prompt for after this turn").assertIsDisplayed()
        compose.onNodeWithContentDescription("Stop").performClick()
        assertEquals(CommandBody.Interrupt(s2), recorder.sent.last())
    }

    @Test
    fun aRefusedPromptLeavesTheQueueAndSaysWhy() {
        show(sampleSession(Stage.Switched), recorder = Recorder(refusal = "the session is archived"))
        composer().performTextInput("one more thing")
        compose.onNodeWithContentDescription("Send").performClick()
        compose.onNodeWithText("the session is archived").assertIsDisplayed()
        compose.onAllNodesWithText("QUEUED").assertCountEquals(0)
    }

    @Test
    fun switchesAndPullRequestsReadAsTheTuiSaysThem() {
        show(sampleSession(Stage.Switched))
        compose.onNodeWithText("switched to codex-work · gpt-5 (transcript replayed)").assertIsDisplayed()
        compose.onNodeWithText("mode set to auto_edit").assertIsDisplayed()
        compose.onNodeWithText("#12 ✓").assertIsDisplayed()
        compose.onNodeWithText("△ denied Bash · by you").assertIsDisplayed()
    }

    @Test
    fun aVaultsSessionIsReadOnly() {
        val vault = sampleMachine().copy(hosts = listOf(sh.herder.ffi.FleetHost("devbox", "devbox", true, Instant.EPOCH.toString())))
        show(sampleSession(Stage.Approval), vault)
        compose.onNodeWithText(
            "A vault's sessions are read-only here: open the session from its own machine to drive it.",
        ).assertIsDisplayed()
        compose.onAllNodesWithText("Allow").assertCountEquals(0)
    }

    @Test
    fun anEmptySessionSaysHowToStart() {
        show(Session().applied(SessionUpdate(emptyList(), emptyList())))
        compose.onNodeWithText("Nothing here yet. Write a prompt to start.").assertIsDisplayed()
    }
}
