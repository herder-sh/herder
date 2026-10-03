package sh.herder.android.ui

import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import com.github.takahirom.roborazzi.captureRoboImage
import java.time.Instant
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import sh.herder.android.sampleFleet

/**
 * Screenshots of the machines and session lists, light and dark, on a phone and a tablet, into
 * `build/outputs/roborazzi` for the `android` workflow to upload. Nothing is compared: they are
 * for people to look at.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ScreenshotsTest {
    @get:Rule
    val compose = createComposeRule()

    private val now = Instant.parse("2026-10-03T12:00:00Z")

    private fun show(dark: Boolean) {
        compose.setContent { HerderTheme(dark = dark) { MachinesScreen(sampleFleet(now), now) } }
    }

    private fun capture(name: String) {
        compose.onRoot().captureRoboImage("build/outputs/roborazzi/$name.png")
    }

    private fun phone(theme: String) {
        show(dark = theme == "dark")
        capture("phone-$theme-machines")
        compose.onNodeWithText("All machines").performClick()
        capture("phone-$theme-sessions-by-project")
        compose.onNodeWithText("By machine").performClick()
        capture("phone-$theme-sessions-by-machine")
    }

    private fun tablet(theme: String) {
        show(dark = theme == "dark")
        capture("tablet-$theme-by-project")
        compose.onNodeWithText("By machine").performClick()
        capture("tablet-$theme-by-machine")
        compose.onNodeWithText("box").performClick()
        capture("tablet-$theme-box")
    }

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneLight() = phone("light")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneDark() = phone("dark")

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletLight() = tablet("light")

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletDark() = tablet("dark")
}
