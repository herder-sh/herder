package sh.herder.android

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.flow.flow
import sh.herder.ffi.Client
import sh.herder.ffi.ConnectionState
import sh.herder.ffi.FailoverSettings
import sh.herder.ffi.Machine

/** What the app shows: the client's machines, or why its profile could not be opened. */
sealed interface Profile {
    data class Open(val machines: List<Machine>) : Profile

    data class Failed(val message: String) : Profile
}

/** The client's machines, now and after every change, until the client stops. */
fun Client.machinesFlow(): Flow<List<Machine>> = flow {
    changes().use { changes ->
        emit(machines())
        while (changes.next()) emit(machines())
    }
}.conflate()

/** A machine's connection, in words. */
fun ConnectionState.label(): String = when (this) {
    ConnectionState.Connected -> "connected"
    ConnectionState.Connecting -> "connecting"
    is ConnectionState.Disconnected -> error
}

/** How many machines are connected, for the title bar. */
fun summary(machines: List<Machine>): String {
    val connected = machines.count { it.connection == ConnectionState.Connected }
    return when (machines.size) {
        0 -> ""
        connected -> "all connected"
        else -> "$connected of ${machines.size} connected"
    }
}

/** A machine with only a name and a connection, for previews and tests. */
internal fun machine(hostId: String, name: String, connection: ConnectionState) = Machine(
    hostId = hostId,
    name = name,
    addresses = emptyList(),
    fingerprint = "",
    connection = connection,
    role = null,
    sessions = emptyList(),
    hosts = emptyList(),
    projects = emptyList(),
    accounts = emptyList(),
    failover = FailoverSettings(pin = false),
    terminals = emptyList(),
    resources = null,
    sessionUsage = emptyMap(),
)
