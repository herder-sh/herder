package sh.herder.android

import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import sh.herder.ffi.ClientInterface

/**
 * Wakes the client when the app comes to the foreground and suspends it when the app goes to
 * the background (client-core's API.md, "App lifecycle"). Observes `ProcessLifecycleOwner`, so
 * moving between activities is neither.
 */
class ClientLifecycle(private val client: ClientInterface) : DefaultLifecycleObserver {
    override fun onStart(owner: LifecycleOwner) {
        client.wake()
    }

    // Saves the offline cache before returning, so it is on disk before the OS may freeze
    // or kill the process.
    override fun onStop(owner: LifecycleOwner) {
        client.suspend()
    }
}
