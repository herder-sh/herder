package sh.herder.android.ui

import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithContentDescription
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onFirst
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import java.time.Instant
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import sh.herder.android.Profile
import sh.herder.android.SampleKey
import sh.herder.android.Session
import sh.herder.android.Stage
import sh.herder.android.machine
import sh.herder.android.sampleFleet
import sh.herder.android.sampleMachine
import sh.herder.android.sampleSession
import sh.herder.ffi.CommandBody
import sh.herder.ffi.ConnectionState
import sh.herder.ffi.SessionStatus

private class PrRecorder {
    val sent = mutableListOf<CommandBody>()
    val opened = mutableListOf<String>()
    val send: Sender = { _, command ->
        sent += command
        null
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w411dp-h891dp")
class PrsScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val now = Instant.parse("2026-10-03T12:00:00Z")
    private val s2 = SampleKey.sessionId

    @Test
    fun theListShowsEverySessionsPrsLiveFirstAndOpensOne() {
        var opened: sh.herder.android.SessionKey? = null
        compose.setContent {
            HerderTheme {
                MachinesScreen(sampleFleet(now), now, onOpenUrl = {}) { key, _, _, _ ->
                    opened = key
                }
            }
        }
        compose.onNodeWithText("Pull requests").performClick()
        compose.onNodeWithText("2 open").assertIsDisplayed()
        compose.onNodeWithText("Add a health endpoint").assertIsDisplayed()
        compose.onNodeWithText("open").assertIsDisplayed()
        compose.onNodeWithText("✓ ci").assertIsDisplayed()
        compose.onNodeWithText("… review").assertIsDisplayed()
        compose.onAllNodesWithText("✓ merge").onFirst().assertIsDisplayed()
        compose.onNodeWithText("Bump axum").assertIsDisplayed()
        compose.onNodeWithText("merged").assertIsDisplayed()
        compose.onNodeWithText("Write the tests").assertIsDisplayed()
        compose.onNodeWithText("draft").assertIsDisplayed()
        compose.onNodeWithText("✗ ci").assertIsDisplayed()
        // Live first: the open PR before the merged one of the same session.
        val texts = listOf("Add a health endpoint", "Bump axum")
        val positions = texts.map { label ->
            compose.onNodeWithText(label).fetchSemanticsNode().boundsInRoot.top
        }
        assertTrue(positions[0] < positions[1])
        compose.onAllNodesWithText("Open").assertCountEquals(3)
        compose.onAllNodesWithText("Open").onFirst().performClick()
        assertEquals(SampleKey, opened)
    }

    @Test
    fun aRowOpensThePrInTheBrowserAndItsMenuUnlinks() {
        val recorder = PrRecorder()
        compose.setContent {
            HerderTheme {
                MachinesScreen(sampleFleet(now), now, recorder.send, recorder.opened::add)
            }
        }
        compose.onNodeWithText("Pull requests").performClick()
        compose.onNodeWithContentDescription("Open https://github.com/org/app/pull/12 in the browser").performClick()
        assertEquals(listOf("https://github.com/org/app/pull/12"), recorder.opened)

        compose.onAllNodesWithContentDescription("More").onFirst().performClick()
        compose.onNodeWithText("Unlink…").performClick()
        compose.onNodeWithText("Unlink #12?").assertIsDisplayed()
        compose.onNodeWithText("Unlink").performClick()
        assertEquals(listOf(CommandBody.UnlinkPr(s2, 12uL)), recorder.sent)
    }

    @Test
    fun groupingByMachineHeadsAVaultsHost() {
        compose.setContent { HerderTheme { MachinesScreen(sampleFleet(now), now) } }
        compose.onNodeWithText("Pull requests").performClick()
        compose.onNodeWithText("By machine").performClick()
        compose.onNodeWithText("laptop").assertIsDisplayed()
        compose.onNodeWithText("Try k3s").assertIsDisplayed()
        compose.onNodeWithText("closed").assertIsDisplayed()
    }

    @Test
    fun saysWhenNoneAreLinked() {
        compose.setContent {
            HerderTheme {
                MachinesScreen(
                    Profile.Open(listOf(machine("h1", "box", ConnectionState.Connected))),
                    now,
                )
            }
        }
        compose.onNodeWithText("none linked").assertIsDisplayed()
        compose.onNodeWithText("Pull requests").performClick()
        compose.onNodeWithText("No pull requests").assertIsDisplayed()
    }

    @Test
    fun theSessionStripOpensLinksAndUnlinks() {
        val recorder = PrRecorder()
        compose.setContent {
            HerderTheme {
                SessionScreen(
                    SampleKey,
                    sampleSession(Stage.Switched),
                    sampleMachine(SessionStatus.IDLE),
                    listOf("opus"),
                    recorder.send,
                    {},
                    {},
                    { now },
                    compact = false,
                    onOpenUrl = recorder.opened::add,
                )
            }
        }
        compose.onNodeWithText("Add a health endpoint").assertIsDisplayed()
        compose.onNodeWithText("open").assertIsDisplayed()
        compose.onNodeWithText("✓ ci").assertIsDisplayed()
        compose.onNodeWithText("… review").assertIsDisplayed()
        compose.onNodeWithText("✓ merge").assertIsDisplayed()
        compose.onNodeWithText("herder/api").assertIsDisplayed()
        compose.onNodeWithText("Document the health endpoint").assertIsDisplayed()
        compose.onNodeWithText("draft").assertIsDisplayed()
        compose.onNodeWithText("… ci").assertIsDisplayed()

        compose.onNodeWithContentDescription("Open https://github.com/org/app/pull/12 in the browser").performClick()
        assertEquals(listOf("https://github.com/org/app/pull/12"), recorder.opened)

        compose.onNodeWithContentDescription("Session").performClick()
        compose.onNodeWithText("Link Pull Request…").performClick()
        compose.onNodeWithText("Link a Pull Request").assertIsDisplayed()
        compose.onNodeWithText("Link").assertIsNotEnabled()
        compose.onNode(hasSetTextAction()).performTextInput("https://github.com/org/app/pull/41")
        compose.onNodeWithText("Link").performClick()
        assertEquals(CommandBody.LinkPr(s2, 41uL), recorder.sent.last())
    }

    @Test
    fun aReadOnlySessionCannotLinkOrUnlink() {
        val recorder = PrRecorder()
        val vault = sampleMachine().copy(
            hosts = listOf(sh.herder.ffi.FleetHost("devbox", "devbox", true, now.toString())),
        )
        compose.setContent {
            HerderTheme {
                SessionScreen(
                    SampleKey,
                    sampleSession(Stage.Switched),
                    vault,
                    listOf("opus"),
                    recorder.send,
                    {},
                    {},
                    { now },
                    onOpenUrl = recorder.opened::add,
                )
            }
        }
        compose.onNodeWithContentDescription("Session").performClick()
        compose.onNodeWithText("Link Pull Request…").assertIsNotEnabled()
        compose.onAllNodesWithContentDescription("More").onFirst().performClick()
        compose.onAllNodesWithText("Unlink…").assertCountEquals(0)
    }

    @Test
    fun aPhoneStripIsOneLine() {
        compose.setContent {
            HerderTheme {
                SessionScreen(
                    SampleKey,
                    sampleSession(Stage.Switched),
                    sampleMachine(SessionStatus.IDLE),
                    listOf("opus"),
                    { _, _ -> null },
                    {},
                    {},
                    { now },
                    compact = true,
                    onOpenUrl = {},
                )
            }
        }
        compose.onNodeWithText("Add a health endpoint").assertIsDisplayed()
        compose.onAllNodesWithText("✓ ci").assertCountEquals(0)
        compose.onAllNodesWithText("herder/api").assertCountEquals(0)
    }
}
