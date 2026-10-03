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
import sh.herder.android.Profile
import sh.herder.android.SampleKey
import sh.herder.android.SampleStart
import sh.herder.android.Stage
import sh.herder.android.Summary
import sh.herder.android.sampleFleet
import sh.herder.android.sampleMachine
import sh.herder.android.sampleSession
import sh.herder.ffi.SessionStatus

/**
 * Screenshots of the session PR strip and the all-PRs screen, light and dark, on a phone and a
 * tablet, into `docs/screenshots/p9-4` and `build/outputs/roborazzi`. Nothing is compared: they
 * are for people to look at.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class PrsScreenshotsTest {
    @get:Rule
    val compose = createComposeRule()

    private val now = Instant.parse("2026-10-03T12:00:00Z")

    private fun capture(name: String) {
        val file = "$name.png"
        compose.onRoot().captureRoboImage("build/outputs/roborazzi/$file")
        compose.onRoot().captureRoboImage("../../docs/screenshots/p9-4/$file")
    }

    private fun phoneAll(theme: String) {
        compose.setContent { HerderTheme(dark = theme == "dark") { MachinesScreen(sampleFleet(now), now) } }
        compose.onNodeWithText("Pull requests").performClick()
        capture("phone-$theme-all")
    }

    private fun phoneSession(theme: String) {
        compose.setContent {
            HerderTheme(dark = theme == "dark") {
                SessionScreen(
                    SampleKey,
                    sampleSession(Stage.Switched),
                    sampleMachine(SessionStatus.IDLE),
                    listOf("opus", "sonnet"),
                    { _, _ -> null },
                    {},
                    {},
                    { SampleStart.plusSeconds(90) },
                    compact = true,
                    onOpenUrl = {},
                )
            }
        }
        capture("phone-$theme-session")
    }

    private fun phoneLink(theme: String) {
        compose.mainClock.autoAdvance = false
        compose.setContent {
            HerderTheme(dark = theme == "dark") {
                LinkPrDialog("api · herder/api", onDismiss = {}, onLink = {})
            }
        }
        compose.mainClock.advanceTimeBy(1_000)
        capture("phone-$theme-link")
    }

    private fun tabletAll(theme: String) {
        compose.setContent { HerderTheme(dark = theme == "dark") { MachinesScreen(sampleFleet(now), now) } }
        compose.onNodeWithText("Pull requests").performClick()
        capture("tablet-$theme-all")
    }

    private fun tabletSession(theme: String) {
        val session = sampleSession(Stage.Switched)
        val profile = Profile.Open(
            listOf(sampleMachine(SessionStatus.IDLE)),
            mapOf(
                SampleKey to Summary(
                    loaded = true,
                    repo = session.repo,
                    branch = session.branch,
                    prs = session.prs,
                    provider = session.provider.orEmpty(),
                    model = session.model,
                ),
            ),
        )
        compose.setContent {
            HerderTheme(dark = theme == "dark") {
                MachinesScreen(profile, SampleStart) { _, _, _, onBack ->
                    SessionScreen(
                        SampleKey,
                        session,
                        profile.machines[0],
                        listOf("opus", "sonnet"),
                        { _, _ -> null },
                        {},
                        onBack,
                        { SampleStart.plusSeconds(90) },
                        compact = false,
                        onOpenUrl = {},
                    )
                }
            }
        }
        compose.onNodeWithText("herder/api").performClick()
        capture("tablet-$theme-session")
    }

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneLightAll() = phoneAll("light")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneDarkAll() = phoneAll("dark")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneLightSession() = phoneSession("light")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneDarkSession() = phoneSession("dark")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneLightLink() = phoneLink("light")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneDarkLink() = phoneLink("dark")

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletLightAll() = tabletAll("light")

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletDarkAll() = tabletAll("dark")

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletLightSession() = tabletSession("light")

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletDarkSession() = tabletSession("dark")
}
