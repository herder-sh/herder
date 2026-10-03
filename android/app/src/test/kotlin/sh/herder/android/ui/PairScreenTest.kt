package sh.herder.android.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import sh.herder.android.Profile
import sh.herder.android.SAMPLE_PAIR_FP
import sh.herder.android.SAMPLE_PAIR_LINK
import sh.herder.android.groupedFingerprint
import sh.herder.android.machine
import sh.herder.ffi.ConnectionState

@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w411dp-h891dp")
class PairScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun pasteShowsTheFingerprintToConfirm() {
        compose.setContent { HerderTheme { PairScreen(onClose = {}) } }
        compose.onNodeWithText("herder pair").assertIsDisplayed()
        compose.onNodeWithText("Pair").assertIsNotEnabled()
        compose.onNodeWithText("herder://pair?host=…&fp=…&code=…").performTextInput(SAMPLE_PAIR_LINK)
        compose.onNodeWithText("192.168.1.5:7447\n10.0.0.2:7447").assertIsDisplayed()
        compose.onNodeWithText(groupedFingerprint(SAMPLE_PAIR_FP)).assertIsDisplayed()
        compose.onNodeWithText("Check it is your machine", substring = true).assertIsDisplayed()
    }

    @Test
    fun rejectsWhatIsNotAPairingLink() {
        compose.setContent { HerderTheme { PairScreen(onClose = {}) } }
        compose.onNodeWithText("herder://pair?host=…&fp=…&code=…").performTextInput("https://herder.sh")
        compose.onNodeWithText("That is not a herder pairing link.").assertIsDisplayed()
        compose.onNodeWithText("Pair").assertIsNotEnabled()
    }

    @Test
    fun warnsWhenTheFingerprintIsAlreadyPaired() {
        compose.setContent {
            HerderTheme {
                PairScreen(
                    machines = listOf(machine("h1", "box", ConnectionState.Connected).copy(fingerprint = SAMPLE_PAIR_FP)),
                    initialLink = SAMPLE_PAIR_LINK,
                    onClose = {},
                )
            }
        }
        compose.onNodeWithText("Already paired as box: pairing again gives it a new key.")
            .performScrollTo()
            .assertIsDisplayed()
    }

    @Test
    fun pairingCallsOnPairWithTheLink() {
        var seen: String? = null
        var closed = false
        compose.setContent {
            HerderTheme {
                PairScreen(
                    initialLink = "noise $SAMPLE_PAIR_LINK trailing",
                    onPair = { seen = it; null },
                    onClose = { closed = true },
                )
            }
        }
        compose.onNodeWithText("Pair").performClick()
        compose.waitForIdle()
        assertEquals(SAMPLE_PAIR_LINK, seen)
        assertTrue(closed)
    }

    @Test
    fun aFailedPairStaysOpenWithTheReason() {
        var closed = false
        compose.setContent {
            HerderTheme {
                PairScreen(
                    initialLink = SAMPLE_PAIR_LINK,
                    onPair = { "pairing failed: connection refused" },
                    onClose = { closed = true },
                )
            }
        }
        compose.onNodeWithText("Pair").performClick()
        compose.waitForIdle()
        compose.onNodeWithText("pairing failed: connection refused").assertIsDisplayed()
        assertEquals(false, closed)
    }

    @Test
    fun scanOpensTheScannerChrome() {
        compose.setContent {
            HerderTheme { PairScreen(scannerPreview = true, onClose = {}) }
        }
        compose.onNodeWithText("Scan QR code").performClick()
        compose.onNodeWithText("Scan the QR code").assertIsDisplayed()
        compose.onNodeWithText("Point the camera at the QR code herder pair prints.").assertIsDisplayed()
        compose.onNodeWithContentDescription("Close").performClick()
        compose.onNodeWithText("Add machine").assertIsDisplayed()
    }

    @Test
    fun theMachinesListOpensAddMachine() {
        compose.setContent {
            HerderTheme { MachinesScreen(Profile.Open(emptyList()), onPair = { null }) }
        }
        compose.onNodeWithText("No machines").assertIsDisplayed()
        compose.onNodeWithText("Add a machine").performClick()
        compose.onNodeWithText("Add machine").assertIsDisplayed()
        compose.onNodeWithText("Scan QR code").assertIsDisplayed()
        compose.onNodeWithContentDescription("Back").performClick()
        compose.onNodeWithText("No machines").assertIsDisplayed()
    }

    @Test
    fun aPairingLinkOpensOnTheConfirmStep() {
        compose.setContent {
            HerderTheme {
                MachinesScreen(Profile.Open(emptyList()), onPair = { null }, initialLink = SAMPLE_PAIR_LINK)
            }
        }
        compose.onNodeWithText("Add machine").assertIsDisplayed()
        compose.onNodeWithText(groupedFingerprint(SAMPLE_PAIR_FP)).assertIsDisplayed()
    }
}
