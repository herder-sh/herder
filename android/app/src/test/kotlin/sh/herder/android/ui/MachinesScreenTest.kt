package sh.herder.android.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import sh.herder.android.Profile
import sh.herder.android.machine
import sh.herder.ffi.ConnectionState

@RunWith(RobolectricTestRunner::class)
class MachinesScreenTest {
    @get:Rule
    val compose = createComposeRule()

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
        compose.onNodeWithText("connected").assertIsDisplayed()
        compose.onNodeWithText("laptop").assertIsDisplayed()
        compose.onNodeWithText("connection refused").assertIsDisplayed()
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
