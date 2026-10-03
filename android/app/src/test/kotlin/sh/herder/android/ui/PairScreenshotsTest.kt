package sh.herder.android.ui

import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.captureRoboImage
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import sh.herder.android.Profile
import sh.herder.android.SAMPLE_PAIR_FP
import sh.herder.android.SAMPLE_PAIR_LINK
import sh.herder.android.machine
import sh.herder.ffi.ConnectionState

/**
 * Screenshots of QR pairing, light and dark, on a phone and a tablet, into
 * `docs/screenshots/p9-6` and `build/outputs/roborazzi`. Nothing is compared: they are for
 * people to look at.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class PairScreenshotsTest {
    @get:Rule
    val compose = createComposeRule()

    private fun capture(name: String) {
        val file = "$name.png"
        compose.onRoot().captureRoboImage("build/outputs/roborazzi/$file")
        compose.onRoot().captureRoboImage("../../docs/screenshots/p9-6/$file")
    }

    private fun add(theme: String) {
        compose.setContent {
            HerderTheme(dark = theme == "dark") { PairScreen(onClose = {}) }
        }
        capture("phone-$theme-add")
    }

    private fun confirm(theme: String) {
        compose.setContent {
            HerderTheme(dark = theme == "dark") {
                PairScreen(
                    machines = listOf(machine("h1", "box", ConnectionState.Connected).copy(fingerprint = SAMPLE_PAIR_FP)),
                    initialLink = SAMPLE_PAIR_LINK,
                    onClose = {},
                )
            }
        }
        capture("phone-$theme-confirm")
    }

    private fun scan(theme: String) {
        compose.setContent {
            HerderTheme(dark = theme == "dark") {
                PairScreen(startScanning = true, scannerPreview = true, onClose = {})
            }
        }
        capture("phone-$theme-scan")
    }

    private fun empty(theme: String) {
        compose.setContent {
            HerderTheme(dark = theme == "dark") { MachinesScreen(Profile.Open(emptyList())) }
        }
        capture("phone-$theme-empty")
    }

    private fun tabletConfirm(theme: String) {
        compose.setContent {
            HerderTheme(dark = theme == "dark") {
                PairScreen(initialLink = SAMPLE_PAIR_LINK, onClose = {})
            }
        }
        capture("tablet-$theme-confirm")
    }

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneLightAdd() = add("light")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneDarkAdd() = add("dark")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneLightConfirm() = confirm("light")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneDarkConfirm() = confirm("dark")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneLightScan() = scan("light")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneDarkScan() = scan("dark")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneLightEmpty() = empty("light")

    @Test
    @Config(qualifiers = "w411dp-h891dp-xxhdpi")
    fun phoneDarkEmpty() = empty("dark")

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletLightConfirm() = tabletConfirm("light")

    @Test
    @Config(qualifiers = "w1280dp-h800dp-xhdpi")
    fun tabletDarkConfirm() = tabletConfirm("dark")
}
