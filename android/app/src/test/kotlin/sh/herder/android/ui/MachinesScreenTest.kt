package sh.herder.android.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import java.time.Instant
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import sh.herder.android.Profile
import sh.herder.android.head
import sh.herder.android.machine
import sh.herder.android.sampleFleet
import sh.herder.ffi.ConnectionState
import sh.herder.ffi.SessionStatus

// A phone, tall enough for the sample fleet's rows to be on screen.
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w411dp-h891dp")
class MachinesScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val now = Instant.parse("2026-10-03T12:00:00Z")

    @Test
    fun showsEachMachineWithItsConnection() {
        compose.setContent {
            HerderTheme {
                MachinesScreen(
                    Profile.Open(
                        listOf(
                            machine("h1", "build-box", ConnectionState.Connected),
                            machine("h2", "laptop", ConnectionState.Disconnected("connection refused")),
                        ),
                    ),
                )
            }
        }
        compose.onNodeWithText("1 of 2 connected").assertIsDisplayed()
        compose.onNodeWithText("build-box").assertIsDisplayed()
        compose.onNodeWithText("connected · 0 sessions").assertIsDisplayed()
        compose.onNodeWithText("laptop").assertIsDisplayed()
        compose.onNodeWithText("connection refused · 0 sessions").assertIsDisplayed()
    }

    @Test
    fun showsAVaultsHostsOnlineOrOffline() {
        compose.setContent { HerderTheme { MachinesScreen(sampleFleet(now), now) } }
        compose.onNodeWithText("online · 1 session").assertIsDisplayed()
        compose.onNodeWithText("offline · 2h 5m ago · 1 session").assertIsDisplayed()
    }

    @Test
    fun pullRequestsOpensEverySessionsPrs() {
        compose.setContent { HerderTheme { MachinesScreen(sampleFleet(now), now) } }
        compose.onNodeWithText("2 open").assertIsDisplayed()
        compose.onNodeWithText("Pull requests").performClick()
        compose.onNodeWithText("Pull requests").assertIsDisplayed()
        compose.onNodeWithText("Add a health endpoint").assertIsDisplayed()
        compose.onNodeWithContentDescription("Back").performClick()
        compose.onNodeWithText("Machines").assertIsDisplayed()
    }

    @Test
    fun aMachineOpensItsSessionsByProjectOrByMachine() {
        compose.setContent { HerderTheme { MachinesScreen(sampleFleet(now), now) } }
        compose.onNodeWithText("box").performClick()

        // A phone is narrow: only each branch's last part.
        compose.onNodeWithText("App").assertIsDisplayed()
        compose.onNodeWithText("api").assertIsDisplayed()
        compose.onNodeWithText("running · box").assertIsDisplayed()
        compose.onNodeWithText("2 tasks").assertIsDisplayed()
        compose.onNodeWithContentDescription("1 need you").assertIsDisplayed()
        compose.onNodeWithText("#12 ✓").assertIsDisplayed()
        compose.onNodeWithText("write the tests").assertIsDisplayed()
        compose.onNodeWithText("needs you · box").assertIsDisplayed()
        compose.onNodeWithText("#14 ✗").assertIsDisplayed()
        // Another machine's sessions are not shown.
        compose.onAllNodesWithText("fix-login").assertCountEquals(0)

        compose.onNodeWithText("By machine").performClick()
        compose.onNodeWithText("app · api").assertIsDisplayed()
        compose.onNodeWithText("connected · 4 sessions").assertIsDisplayed()

        compose.onNodeWithContentDescription("Back").performClick()
        compose.onNodeWithText("Machines").assertIsDisplayed()
    }

    @Test
    fun theListFollowsLiveChanges() {
        var profile by mutableStateOf(
            Profile.Open(listOf(machine("h1", "box", ConnectionState.Connected, listOf(head("s1", null))))),
        )
        compose.setContent { HerderTheme { MachinesScreen(profile, now) } }
        compose.onNodeWithText("All machines").performClick()
        compose.onNodeWithText("idle · box").assertIsDisplayed()

        profile = Profile.Open(
            listOf(
                machine(
                    "h1", "box", ConnectionState.Connected,
                    listOf(head("s1", null).copy(status = SessionStatus.NEEDS_YOU)),
                ),
            ),
        )
        compose.onNodeWithText("needs you · box").assertIsDisplayed()
    }

    @Test
    @Config(qualifiers = "w1280dp-h800dp")
    fun aTabletShowsTheMachinesBesideTheSessions() {
        compose.setContent { HerderTheme { MachinesScreen(sampleFleet(now), now) } }
        compose.onNodeWithText("2 of 3 connected").assertIsDisplayed()
        // Wide, rows show the whole branch and headings where their sessions run.
        compose.onNodeWithText("herder/api").assertIsDisplayed()
        compose.onNodeWithText("4 sessions · box, nas").assertIsDisplayed()

        compose.onNodeWithText("laptop").performClick()
        compose.onNodeWithText("herder/docs").assertIsDisplayed()
        compose.onAllNodesWithText("herder/api").assertCountEquals(0)
    }

    @Test
    fun saysWhenThereAreNoMachines() {
        compose.setContent { HerderTheme { MachinesScreen(Profile.Open(emptyList())) } }
        compose.onNodeWithText("No machines").assertIsDisplayed()
    }

    @Test
    fun saysWhyTheProfileCouldNotBeOpened() {
        compose.setContent { HerderTheme { MachinesScreen(Profile.Failed("disk full")) } }
        compose.onNodeWithText("Cannot open the profile").assertIsDisplayed()
        compose.onNodeWithText("disk full").assertIsDisplayed()
    }
}
