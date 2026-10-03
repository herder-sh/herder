package sh.herder.android

import android.app.Application
import androidx.lifecycle.ProcessLifecycleOwner
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.stateIn
import sh.herder.ffi.Client
import sh.herder.ffi.HerderException

/**
 * Opens the client on this device's profile, in app-private storage, for the life of the
 * process, and tells it when the app goes to the background and comes back. The lifecycle
 * observer and the fleet flow hold the client: releasing it would stop every connection.
 */
class HerderApp : Application() {
    /** The client's machines and their sessions, live, or why the profile could not be opened. */
    lateinit var profile: StateFlow<Profile>
        private set

    override fun onCreate() {
        super.onCreate()
        val client = try {
            Client.open(filesDir.resolve("herder").path, "herder-android/${BuildConfig.VERSION_NAME}")
        } catch (error: HerderException) {
            profile = MutableStateFlow(Profile.Failed((error as? HerderException.Local)?.detail ?: error.toString()))
            return
        }
        profile = fleetFlow(client.machinesFlow(), client::updates)
            .stateIn(MainScope(), SharingStarted.Eagerly, Profile.Open(client.machines()))
        ProcessLifecycleOwner.get().lifecycle.addObserver(ClientLifecycle(client))
    }
}
