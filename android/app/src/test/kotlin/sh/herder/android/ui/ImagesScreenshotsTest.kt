package sh.herder.android.ui

import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import com.github.takahirom.roborazzi.captureRoboImage
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
import sh.herder.android.Summary
import sh.herder.android.firstTurn
import sh.herder.android.sampleImageSession
import sh.herder.android.sampleMachine
import sh.herder.android.samplePng
import sh.herder.ffi.Image
import sh.herder.ffi.SessionStatus

/**
 * Screenshots of attaching and viewing images, light and dark, on a phone and a tablet, into
 * `docs/screenshots/p9-8` and `build/outputs/roborazzi`. Nothing is compared: they are for
 * people to look at.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ImagesScreenshotsTest {
    @get:Rule
    val compose = createComposeRule()

    private val clock = { SampleStart.plusSeconds(90) }
    private val photo = samplePng()

    private fun capture(name: String) {
        val file = "$name.png"
        compose.onRoot().captureRoboImage("build/outputs/roborazzi/$file")
        compose.onRoot().captureRoboImage("../../docs/screenshots/p9-8/$file")
    }

    private fun phoneComposer(theme: String) {
        compose.setContent {
            HerderTheme(dark = theme == "dark") {
                SessionScreen(
                    SampleKey,
                    Session().applied(firstTurn()),
                    sampleMachine(SessionStatus.IDLE),
                    listOf("opus", "sonnet"),
                    { _, _ -> null },
                    {},
                    {},
                    clock,
                    compact = true,
                    initialImages = listOf(Image("image/png", photo), Image("image/png", samplePng(0xFF3D5A80.toInt(), 0xFFE0FBFC.toInt()))),
                )
            }
        }
        capture("phone-$theme-composer")
    }

    private fun phoneTranscript(theme: String) {
        compose.setContent {
            HerderTheme(dark = theme == "dark") {
                SessionScreen(
                    SampleKey,
                    sampleImageSession(),
                    sampleMachine(SessionStatus.IDLE),
                    listOf("opus", "sonnet"),
                    { _, _ -> null },
                    {},
                    {},
                    clock,
                    compact = true,
                    fetchAttachment = { photo },
                )
            }
        }
        capture("phone-$theme-transcript")
    }

    private fun phoneMissing(theme: String) {
        compose.setContent {
            HerderTheme(dark = theme == "dark") {
                SessionScreen(
                    SampleKey,
                    sampleImageSession(),
                    sampleMachine(SessionStatus.IDLE),
                    listOf("opus", "sonnet"),
                    { _, _ -> null },
                    {},
                    {},
                    clock,
                    compact = true,
                )
            }
        }
        capture("phone-$theme-missing")
    }

    private fun phoneViewer(theme: String) {
        compose.setContent {
            HerderTheme(dark = theme == "dark") {
                SessionScreen(
                    SampleKey,
                    sampleImageSession(),
                    sampleMachine(SessionStatus.IDLE),
                    listOf("opus", "sonnet"),
                    { _, _ -> null },
                    {},
                    {},
                    clock,
                    compact = true,
                    fetchAttachment = { photo },
                )
            }
        }
        compose.onNodeWithContentDescription("Image 1").performClick()
        compose.onNodeWithContentDescription("Full-size image").captureRoboImage("build/outputs/roborazzi/phone-$theme-viewer.png")
        compose.onNodeWithContentDescription("Full-size image").captureRoboImage("../../docs/screenshots/p9-8/phone-$theme-viewer.png")
    }

    private fun tabletTranscript(theme: String) {
        val session = sampleImageSession()
        val profile = Profile.Open(
            listOf(sampleMachine(SessionStatus.IDLE)),
            mapOf(SampleKey to Summary().applied(firstTurn())),
        )
        compose.setContent {
            HerderTheme(dark = theme == "dark") {
                MachinesScreen(profile, SampleStart) { key, _, _, onBack ->
                    SessionScreen(
                        key,
                        session,
                        profile.machines[0],
                        listOf("opus", "sonnet"),
                        { _, _ -> null },
                        {},
                        onBack,
                        clock,
                        fetchAttachment = { photo },
                    )
                }
            }
        }
        compose.onNodeWithText("herder/api").performClick()
        capture("tablet-$theme-transcript")
    }

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneLightComposer() = phoneComposer("light")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneDarkComposer() = phoneComposer("dark")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneLightTranscript() = phoneTranscript("light")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneDarkTranscript() = phoneTranscript("dark")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneLightMissing() = phoneMissing("light")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneDarkMissing() = phoneMissing("dark")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneLightViewer() = phoneViewer("light")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneDarkViewer() = phoneViewer("dark")

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletLightTranscript() = tabletTranscript("light")

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletDarkTranscript() = tabletTranscript("dark")

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletLightComposer() {
        compose.setContent {
            HerderTheme {
                SessionScreen(
                    SampleKey,
                    Session().applied(firstTurn()),
                    sampleMachine(SessionStatus.IDLE),
                    listOf("opus", "sonnet"),
                    { _, _ -> null },
                    {},
                    {},
                    clock,
                    initialImages = listOf(Image("image/png", photo)),
                )
            }
        }
        capture("tablet-light-composer")
    }

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletDarkComposer() {
        compose.setContent {
            HerderTheme(dark = true) {
                SessionScreen(
                    SampleKey,
                    Session().applied(firstTurn()),
                    sampleMachine(SessionStatus.IDLE),
                    listOf("opus", "sonnet"),
                    { _, _ -> null },
                    {},
                    {},
                    clock,
                    initialImages = listOf(Image("image/png", photo)),
                )
            }
        }
        capture("tablet-dark-composer")
    }
}
