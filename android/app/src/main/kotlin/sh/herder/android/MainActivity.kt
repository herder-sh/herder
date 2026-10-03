package sh.herder.android

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.flow.runningFold
import sh.herder.android.ui.HerderTheme
import sh.herder.android.ui.MachinesScreen
import sh.herder.android.ui.SessionScreen
import sh.herder.ffi.Client
import sh.herder.ffi.HerderException

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val app = application as HerderApp
        setContent {
            HerderTheme {
                val state by app.profile.collectAsStateWithLifecycle()
                MachinesScreen(
                    state,
                    send = { host, command ->
                        try {
                            app.client?.send(host, command)
                            null
                        } catch (error: HerderException) {
                            error.reason()
                        }
                    },
                    session = { key, compact, onOpen, onBack ->
                        app.client?.let { LiveSession(it, state, key, compact, onOpen, onBack) }
                    },
                )
            }
        }
    }
}

/**
 * The session [key], followed through its own subscription while it is open, with the machine
 * and the models the [profile]'s other sessions use.
 */
@Composable
private fun LiveSession(
    client: Client,
    profile: Profile,
    key: SessionKey,
    compact: Boolean,
    onOpen: (SessionKey) -> Unit,
    onBack: () -> Unit,
) {
    val updates = remember(key) {
        client.updates(key).runningFold(Session()) { session, update -> session.applied(update) }.flowOn(Dispatchers.Default)
    }
    val session by updates.collectAsState(Session())
    val open = profile as? Profile.Open
    SessionScreen(
        key = key,
        session = session,
        machine = open?.machines?.find { it.hostId == key.hostId },
        recent = open?.summaries?.let { recentModels(it, session.provider) }.orEmpty(),
        send = { host, command ->
            try {
                client.send(host, command)
                null
            } catch (error: HerderException) {
                error.reason()
            }
        },
        onOpen = onOpen,
        onBack = onBack,
        compact = compact,
    )
}
