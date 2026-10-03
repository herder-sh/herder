package sh.herder.android

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.onCompletion
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test
import sh.herder.ffi.ConnectionState
import sh.herder.ffi.EventBody
import sh.herder.ffi.SessionHead
import sh.herder.ffi.SessionStatus
import sh.herder.ffi.SessionUpdate

class FleetTest {
    @Test
    @OptIn(ExperimentalCoroutinesApi::class) // UnconfinedTestDispatcher
    fun followsEveryListedSessionLiveAndDropsTheOnesThatGo() = runTest {
        val s1 = SessionKey("h1", "s1")
        val s2 = SessionKey("h1", "s2")
        fun box(vararg sessions: SessionHead) =
            listOf(machine("h1", "box", ConnectionState.Connected, sessions.toList()))
        val machines = MutableStateFlow(box(head("s1", null), head("s2", null)))
        val updates = mutableMapOf<SessionKey, MutableSharedFlow<SessionUpdate>>()
        val unsubscribed = mutableListOf<SessionKey>()
        val seen = mutableListOf<Profile.Open>()
        val collector = launch(UnconfinedTestDispatcher(testScheduler)) {
            fleetFlow(machines) { key ->
                MutableSharedFlow<SessionUpdate>()
                    .also { updates[key] = it }
                    .onCompletion { unsubscribed += key }
            }.collect { seen += it }
        }

        assertEquals(emptyMap<SessionKey, Summary>(), seen.last().summaries)
        assertEquals(setOf(s1, s2), updates.keys)

        updates.getValue(s1).emit(update("s1", created("/srv/app", "herder/a")))
        assertEquals("herder/a", seen.last().summaries.getValue(s1).branch)

        // What the lists do not show changes nothing.
        val shown = seen.size
        updates.getValue(s1).emit(update("s1", EventBody.TurnStarted("t1")))
        assertEquals(shown, seen.size)

        // A status change comes with the machine list.
        machines.value = box(head("s1", null).copy(status = SessionStatus.NEEDS_YOU), head("s2", null))
        assertEquals(SessionStatus.NEEDS_YOU, seen.last().machines.single().sessions[0].status)
        assertEquals("herder/a", seen.last().summaries.getValue(s1).branch)

        // A session no longer listed is unsubscribed and forgotten.
        machines.value = box(head("s2", null))
        assertEquals(listOf(s1), unsubscribed)
        assertEquals(emptyMap<SessionKey, Summary>(), seen.last().summaries)
        assertEquals(listOf("s2"), seen.last().machines.single().sessions.map { it.sessionId })

        collector.cancel()
    }
}
