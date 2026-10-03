package sh.herder.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import sh.herder.android.Profile
import sh.herder.android.label
import sh.herder.android.machine
import sh.herder.android.summary
import sh.herder.ffi.ConnectionState
import sh.herder.ffi.Machine

/** The paired machines, each with its connection state. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MachinesScreen(profile: Profile) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("Machines")
                        val summary = (profile as? Profile.Open)?.let { summary(it.machines) }
                        if (!summary.isNullOrEmpty()) {
                            Text(summary, style = MaterialTheme.typography.labelMedium)
                        }
                    }
                },
            )
        },
    ) { padding ->
        when (profile) {
            is Profile.Failed -> Status("Cannot open the profile", profile.message, padding)
            is Profile.Open if profile.machines.isEmpty() ->
                Status("No machines", "Paired machines show up here.", padding)
            is Profile.Open -> LazyColumn(contentPadding = padding) {
                items(profile.machines, key = { it.hostId }) { MachineRow(it) }
            }
        }
    }
}

@Composable
private fun MachineRow(machine: Machine) {
    ListItem(
        headlineContent = { Text(machine.name) },
        supportingContent = { Text(machine.connection.label()) },
        leadingContent = {
            Box(Modifier.size(12.dp).background(machine.connection.color(), CircleShape))
        },
    )
}

@Composable
private fun ConnectionState.color(): Color = when (this) {
    ConnectionState.Connected -> MaterialTheme.colorScheme.primary
    ConnectionState.Connecting -> MaterialTheme.colorScheme.outline
    is ConnectionState.Disconnected -> MaterialTheme.colorScheme.error
}

@Composable
private fun Status(title: String, detail: String, padding: PaddingValues) {
    Column(
        modifier = Modifier.fillMaxSize().padding(padding).padding(32.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(title, style = MaterialTheme.typography.titleLarge)
        Text(detail, style = MaterialTheme.typography.bodyMedium, textAlign = TextAlign.Center)
    }
}

@Preview
@Composable
private fun MachinesPreview() {
    HerderTheme {
        MachinesScreen(
            Profile.Open(
                listOf(
                    machine("h1", "build-box", ConnectionState.Connected),
                    machine("h2", "laptop", ConnectionState.Disconnected("connection refused")),
                ),
            ),
        )
    }
}
