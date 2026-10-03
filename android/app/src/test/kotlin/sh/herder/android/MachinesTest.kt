package sh.herder.android

import org.junit.Assert.assertEquals
import org.junit.Test
import sh.herder.ffi.ConnectionState

class MachinesTest {
    @Test
    fun summaryCountsConnectedMachines() {
        val up = machine("h1", "box", ConnectionState.Connected)
        val down = machine("h2", "laptop", ConnectionState.Disconnected("connection refused"))
        assertEquals("", summary(emptyList()))
        assertEquals("all connected", summary(listOf(up)))
        assertEquals("1 of 2 connected", summary(listOf(up, down)))
    }

    @Test
    fun aDisconnectedMachineShowsWhy() {
        assertEquals("connecting", ConnectionState.Connecting.label())
        assertEquals("connection refused", ConnectionState.Disconnected("connection refused").label())
    }
}
