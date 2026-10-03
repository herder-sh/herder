package sh.herder.android.ui

import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import com.github.takahirom.roborazzi.captureRoboImage
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import sh.herder.android.Profile
import sh.herder.android.SampleKey
import sh.herder.android.SampleStart
import sh.herder.android.Session
import sh.herder.android.Stage
import sh.herder.android.Summary
import sh.herder.android.firstTurn
import sh.herder.android.sampleMachine
import sh.herder.android.sampleSession
import sh.herder.ffi.SessionStatus

/**
 * Screenshots of the session view at each stage of a turn, light and dark, on a phone and a
 * tablet, into `build/outputs/roborazzi` for the `android` workflow to upload. Nothing is
 * compared: they are for people to look at.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class SessionScreenshotsTest {
    @get:Rule
    val compose = createComposeRule()

    private val clock = { SampleStart.plusSeconds(90) }

    private fun status(stage: Stage) = when (stage) {
        Stage.Chat -> SessionStatus.RUNNING
        Stage.Approval, Stage.Question -> SessionStatus.NEEDS_YOU
        Stage.Switched -> SessionStatus.IDLE
    }

    /** The phone's session page, as opened from the list, showing [session] on [machine]. */
    private var session by mutableStateOf(Session())
    private var machine by mutableStateOf(sampleMachine())

    private fun phone(theme: String) {
        compose.setContent {
            HerderTheme(dark = theme == "dark") {
                SessionScreen(SampleKey, session, machine, listOf("opus", "sonnet"), { _, _ -> null }, {}, {}, clock, compact = true)
            }
        }
    }

    private fun show(stage: Stage) {
        session = sampleSession(stage)
        machine = sampleMachine(status(stage))
        compose.waitForIdle()
    }

    private fun capture(name: String) {
        compose.onRoot().captureRoboImage("build/outputs/roborazzi/session-$name.png")
    }

    private fun phoneStages(theme: String) {
        phone(theme)
        for (stage in Stage.entries) {
            show(stage)
            capture("phone-$theme-${stage.name.lowercase()}")
        }
    }

    private fun phonePickers(theme: String) {
        phone(theme)
        session = Session().applied(firstTurn())
        machine = sampleMachine(SessionStatus.IDLE)
        compose.onNodeWithText("cargo test --workspace").performClick()
        capture("phone-$theme-expanded")
        show(Stage.Switched)
        compose.onNodeWithContentDescription("Account: codex-work").performClick()
        capture("phone-$theme-picker-account")
        compose.onNodeWithText("Codex (work)").performClick()
        compose.onNodeWithContentDescription("Model: gpt-5").performClick()
        capture("phone-$theme-picker-model")
        compose.onNodeWithText("Cancel").performClick()
        compose.onNodeWithContentDescription("Mode: auto_edit").performClick()
        capture("phone-$theme-picker-mode")
    }

    /** The tablet: the machines beside the session, opened from its row. */
    private fun tablet(theme: String, stage: Stage) {
        val profile = Profile.Open(
            listOf(sampleMachine(status(stage))),
            mapOf(SampleKey to Summary().applied(firstTurn())),
        )
        compose.setContent {
            HerderTheme(dark = theme == "dark") {
                MachinesScreen(profile, SampleStart) { key, _, _, onBack ->
                    SessionScreen(key, sampleSession(stage), profile.machines[0], listOf("opus", "sonnet"), { _, _ -> null }, {}, onBack, clock)
                }
            }
        }
        compose.onNodeWithText("herder/api").performClick()
        capture("tablet-$theme-${stage.name.lowercase()}")
    }

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneLight() = phoneStages("light")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneDark() = phoneStages("dark")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phonePickersLight() = phonePickers("light")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phonePickersDark() = phonePickers("dark")

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletLight() {
        tablet("light", Stage.Chat)
    }

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletLightApproval() {
        tablet("light", Stage.Approval)
    }

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletDark() {
        tablet("dark", Stage.Chat)
    }

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletDarkQuestion() {
        tablet("dark", Stage.Question)
    }
}
