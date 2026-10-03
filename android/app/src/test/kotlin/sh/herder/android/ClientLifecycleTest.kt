package sh.herder.android

import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.LifecycleRegistry
import java.lang.reflect.Proxy
import org.junit.Assert.assertEquals
import org.junit.Test
import sh.herder.ffi.ClientInterface

class ClientLifecycleTest {
    @Test
    fun suspendsInTheBackgroundAndWakesInTheForeground() {
        val calls = mutableListOf<String>()
        // Records every call; the lifecycle only calls wake() and suspend(), which return nothing.
        val client = Proxy.newProxyInstance(
            ClientInterface::class.java.classLoader,
            arrayOf(ClientInterface::class.java),
        ) { _, method, _ -> calls += method.name; null } as ClientInterface
        val owner = object : LifecycleOwner {
            override val lifecycle = LifecycleRegistry.createUnsafe(this)
        }
        owner.lifecycle.addObserver(ClientLifecycle(client))

        owner.lifecycle.currentState = Lifecycle.State.RESUMED
        assertEquals(listOf("wake"), calls)
        owner.lifecycle.currentState = Lifecycle.State.CREATED
        assertEquals(listOf("wake", "suspend"), calls)
        owner.lifecycle.currentState = Lifecycle.State.RESUMED
        assertEquals(listOf("wake", "suspend", "wake"), calls)
    }
}
