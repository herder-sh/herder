package sh.herder.android

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import sh.herder.android.ui.HerderTheme
import sh.herder.android.ui.MachinesScreen

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val profile = (application as HerderApp).profile
        setContent {
            HerderTheme {
                val state by profile.collectAsStateWithLifecycle()
                MachinesScreen(state)
            }
        }
    }
}
