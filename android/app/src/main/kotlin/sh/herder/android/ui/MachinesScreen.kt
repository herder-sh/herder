package sh.herder.android.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.VerticalDivider
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.Instant
import sh.herder.android.Group
import sh.herder.android.Grouping
import sh.herder.android.Lists
import sh.herder.android.Profile
import sh.herder.android.R
import sh.herder.android.Scope
import sh.herder.android.SessionKey
import sh.herder.android.SessionRow
import sh.herder.android.badge
import sh.herder.android.glyph
import sh.herder.android.hostState
import sh.herder.android.keys
import sh.herder.android.label
import sh.herder.android.sampleFleet
import sh.herder.android.scopeTitle
import sh.herder.android.scopes
import sh.herder.android.sessions
import sh.herder.android.summary
import sh.herder.ffi.ConnectionState
import sh.herder.ffi.Machine
import sh.herder.ffi.PrState
import sh.herder.ffi.PullRequest
import sh.herder.ffi.SessionStatus

/** From this width the machines and the sessions sit side by side; below it they are pages. */
private val TwoPanes = 600.dp

private val MachinesPaneWidth = 320.dp

/** How far a vault's host or a task's child is indented per level. */
private val Indent = 20.dp

/** Most PRs a session row names; the rest are counted. */
private const val ROW_PRS = 2

/**
 * Draws the open session [key]: [onOpen] opens another, [onBack] returns to where it was opened
 * from; [compact] on a phone.
 */
typealias SessionContent = @Composable (key: SessionKey, compact: Boolean, onOpen: (SessionKey) -> Unit, onBack: () -> Unit) -> Unit

/**
 * The paired machines, each with its connection and a vault's hosts under it, online or
 * offline; and the sessions of the selected one, or of all, grouped by project or by machine,
 * each with its status, its task tree and its PRs. Wide, the two sit side by side; narrow, the
 * sessions are a page of their own and rows show less, as the TUI's compact rows do. A session
 * opens on a tap, drawn by [session]: in place of the list beside the machines, or as a page.
 */
@Composable
fun MachinesScreen(
    profile: Profile,
    now: Instant = remember(profile) { Instant.now() },
    session: SessionContent = { _, _, _, _ -> },
) {
    // What the user picked; on a phone, `null` shows the machines page.
    var picked by remember { mutableStateOf<Scope?>(null) }
    // The sessions opened, the last one shown; a child opened from its parent goes on top.
    var opened by remember { mutableStateOf<List<SessionKey>>(emptyList()) }
    val back = { opened = opened.dropLast(1) }
    val open: (SessionKey) -> Unit = { opened = opened + it }
    var grouping by rememberSaveable { mutableStateOf(Grouping.Projects) }
    val listed = (profile as? Profile.Open)?.takeIf { it.machines.isNotEmpty() }
    // A machine or host gone from the list falls back to all machines.
    val scope = picked?.let { if (listed != null && it in scopes(listed.machines)) it else Scope.All }
    val shown = opened.lastOrNull()
    BoxWithConstraints(Modifier.fillMaxSize()) {
        when {
            listed == null -> MachinesPane(profile, selected = null, now = now, onSelect = {})
            maxWidth >= TwoPanes -> Row(Modifier.fillMaxSize()) {
                Box(Modifier.width(MachinesPaneWidth).fillMaxHeight()) {
                    MachinesPane(
                        listed,
                        selected = scope ?: Scope.All,
                        now = now,
                        onSelect = {
                            picked = it
                            opened = emptyList()
                        },
                    )
                }
                VerticalDivider()
                if (shown != null) {
                    BackHandler(onBack = back)
                    session(shown, false, open, back)
                } else {
                    SessionsPane(listed, scope ?: Scope.All, grouping, { grouping = it }, compact = false, now, null, open)
                }
            }
            shown != null -> {
                BackHandler(onBack = back)
                session(shown, true, open, back)
            }
            scope == null -> MachinesPane(listed, selected = null, now = now, onSelect = { picked = it })
            else -> {
                BackHandler { picked = null }
                SessionsPane(listed, scope, grouping, { grouping = it }, compact = true, now, { picked = null }, open)
            }
        }
    }
}

/** The machines list, or why there is none; [selected] is highlighted. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MachinesPane(profile: Profile, selected: Scope?, now: Instant, onSelect: (Scope) -> Unit) {
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
                item {
                    ScopeItem(
                        title = "All machines",
                        subtitle = sessions(keys(profile.machines).size),
                        mark = null,
                        depth = 0,
                        selected = selected == Scope.All,
                        onClick = { onSelect(Scope.All) },
                    )
                }
                for (machine in profile.machines) {
                    item(key = machine.hostId) { MachineItem(machine, selected, onSelect) }
                    items(machine.hosts, key = { "${machine.hostId}/${it.hostId}" }) { host ->
                        val scope = Scope.Host(machine.hostId, host.hostId)
                        val count = machine.sessions.count { it.hostId == host.hostId }
                        ScopeItem(
                            title = host.hostName,
                            subtitle = "${hostState(host, now)} · ${sessions(count)}",
                            mark = if (host.online) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.error,
                            depth = 1,
                            selected = selected == scope,
                            onClick = { onSelect(scope) },
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun MachineItem(machine: Machine, selected: Scope?, onSelect: (Scope) -> Unit) {
    val scope = Scope.Machine(machine.hostId)
    ScopeItem(
        title = machine.name,
        subtitle = "${machine.connection.label()} · ${sessions(machine.sessions.size)}",
        mark = machine.connection.color(),
        depth = 0,
        selected = selected == scope,
        onClick = { onSelect(scope) },
    )
}

/** A row of the machines list: a coloured mark, or a ring for all machines, a name and its state. */
@Composable
private fun ScopeItem(
    title: String,
    subtitle: String,
    mark: Color?,
    depth: Int,
    selected: Boolean,
    onClick: () -> Unit,
) {
    ListItem(
        headlineContent = { Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        supportingContent = { Text(subtitle, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        leadingContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Spacer(Modifier.width(Indent * depth))
                val dot = Modifier.size(10.dp)
                Box(
                    if (mark == null) {
                        dot.border(1.5.dp, MaterialTheme.colorScheme.outline, CircleShape)
                    } else {
                        dot.background(mark, CircleShape)
                    },
                )
            }
        },
        colors = ListItemDefaults.colors(
            containerColor = if (selected) MaterialTheme.colorScheme.secondaryContainer else Color.Transparent,
        ),
        modifier = Modifier
            .padding(horizontal = 12.dp)
            .clip(RoundedCornerShape(28.dp))
            .clickable(onClick = onClick),
    )
}

/** The sessions [scope] covers, grouped by [grouping]; [onBack] returns to the machines page. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SessionsPane(
    profile: Profile.Open,
    scope: Scope,
    grouping: Grouping,
    onGrouping: (Grouping) -> Unit,
    compact: Boolean,
    now: Instant,
    onBack: (() -> Unit)?,
    onOpen: (SessionKey) -> Unit,
) {
    val groups = remember(profile, scope, grouping, compact, now) {
        Lists(profile.machines, profile.summaries, compact, now).groups(scope, grouping)
    }
    val count = groups.sumOf { it.rows.size }
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(scopeTitle(profile.machines, scope), maxLines = 1, overflow = TextOverflow.Ellipsis)
                        Text(sessions(count), style = MaterialTheme.typography.labelMedium)
                    }
                },
                navigationIcon = {
                    if (onBack != null) {
                        IconButton(onClick = onBack) {
                            Icon(painterResource(R.drawable.ic_arrow_back), contentDescription = "Back")
                        }
                    }
                },
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding), horizontalAlignment = Alignment.CenterHorizontally) {
            GroupingButtons(grouping, onGrouping)
            if (count == 0) {
                Status("No sessions", "Sessions started on these machines show up here.", PaddingValues())
            } else {
                LazyColumn(
                    modifier = Modifier.weight(1f).widthIn(max = 840.dp).fillMaxWidth(),
                    contentPadding = PaddingValues(bottom = 16.dp),
                ) {
                    for (group in groups) {
                        item { GroupHeader(group) }
                        items(group.rows, key = { "${it.key.hostId}/${it.key.sessionId}" }) { row ->
                            SessionItem(row) { onOpen(row.key) }
                        }
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun GroupingButtons(grouping: Grouping, onGrouping: (Grouping) -> Unit) {
    // Wide enough for either label beside the selected one's check mark.
    SingleChoiceSegmentedButtonRow(Modifier.padding(horizontal = 16.dp, vertical = 8.dp).width(360.dp)) {
        Grouping.entries.forEachIndexed { index, option ->
            SegmentedButton(
                selected = grouping == option,
                onClick = { onGrouping(option) },
                shape = SegmentedButtonDefaults.itemShape(index, Grouping.entries.size),
            ) {
                Text(
                    when (option) {
                        Grouping.Projects -> "By project"
                        Grouping.Machines -> "By machine"
                    },
                )
            }
        }
    }
}

/** A group's heading: the state its sessions roll up to, its name and what it holds. */
@Composable
private fun GroupHeader(group: Group) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 20.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        StatusGlyph(group.status, Modifier.padding(end = 16.dp))
        Column {
            Text(group.title, style = MaterialTheme.typography.titleSmall)
            Text(
                group.description,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/**
 * A session: its status, title and where it runs; how many tasks it spawned, how many of them
 * need the user, and its PRs.
 */
@Composable
private fun SessionItem(row: SessionRow, onClick: () -> Unit) {
    ListItem(
        modifier = Modifier.clickable(onClick = onClick),
        headlineContent = { Text(row.title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        supportingContent = {
            val status = row.status.color()
            val text = buildAnnotatedString {
                val weight = if (row.status == SessionStatus.NEEDS_YOU) FontWeight.SemiBold else null
                withStyle(SpanStyle(color = status, fontWeight = weight)) { append(row.status.label()) }
                row.place?.let { append(" · $it") }
                row.movedTo?.let { append(" · moved to $it") }
            }
            Text(text, maxLines = 1, overflow = TextOverflow.Ellipsis)
        },
        leadingContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Spacer(Modifier.width(Indent * row.depth))
                StatusGlyph(row.status)
            }
        },
        trailingContent = if (row.children == 0 && row.needYou == 0 && row.prs.isEmpty()) {
            null
        } else {
            { Trailing(row) }
        },
    )
}

@Composable
private fun Trailing(row: SessionRow) {
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
        if (row.children > 0) {
            Text(
                if (row.children == 1) "1 task" else "${row.children} tasks",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        if (row.needYou > 0) {
            Pill(
                "!${row.needYou}",
                MaterialTheme.colorScheme.onPrimary,
                fill = MaterialTheme.colorScheme.primary,
                modifier = Modifier.semantics { contentDescription = "${row.needYou} need you" },
            )
        }
        row.prs.take(ROW_PRS).forEach { PrPill(it) }
        if (row.prs.size > ROW_PRS) {
            Text(
                "+${row.prs.size - ROW_PRS}",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/** A PR in its state's colour: open, draft, merged or closed. */
@Composable
internal fun PrPill(pr: PullRequest) {
    val color = when (pr.state) {
        PrState.OPEN -> MaterialTheme.colorScheme.primary
        PrState.DRAFT -> MaterialTheme.colorScheme.onSurfaceVariant
        PrState.MERGED -> MaterialTheme.colorScheme.tertiary
        PrState.CLOSED -> MaterialTheme.colorScheme.error
    }
    Pill(badge(pr), color, fill = null)
}

/** A short label in a rounded outline, or on [fill]. */
@Composable
private fun Pill(text: String, color: Color, fill: Color?, modifier: Modifier = Modifier) {
    val shape = RoundedCornerShape(8.dp)
    val box = if (fill == null) modifier.border(1.dp, color, shape) else modifier.background(fill, shape)
    Text(
        text,
        style = MaterialTheme.typography.labelMedium,
        color = color,
        maxLines = 1,
        modifier = box.padding(horizontal = 6.dp, vertical = 2.dp),
    )
}

/** A status as the TUI's glyph, in its colour; blank for none. */
@Composable
private fun StatusGlyph(status: SessionStatus?, modifier: Modifier = Modifier) {
    Text(
        status?.glyph().orEmpty(),
        // The symbol font draws geometric shapes small at text sizes.
        style = MaterialTheme.typography.titleLarge.copy(fontSize = 22.sp),
        color = status?.color() ?: Color.Unspecified,
        textAlign = TextAlign.Center,
        modifier = modifier.width(GlyphWidth),
    )
}

private val GlyphWidth: Dp = 24.dp

/** A status's colour: the accent for what needs the user, quiet for what does not. */
@Composable
internal fun SessionStatus.color(): Color = when (this) {
    SessionStatus.NEEDS_YOU -> MaterialTheme.colorScheme.primary
    SessionStatus.ERROR -> MaterialTheme.colorScheme.error
    SessionStatus.RUNNING -> MaterialTheme.colorScheme.tertiary
    SessionStatus.WAITING_FOR_CAPACITY, SessionStatus.MOVED -> MaterialTheme.colorScheme.secondary
    SessionStatus.IDLE, SessionStatus.ARCHIVED, SessionStatus.UNKNOWN -> MaterialTheme.colorScheme.onSurfaceVariant
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

@Preview(widthDp = 411, heightDp = 891)
@Composable
private fun PhonePreview() {
    HerderTheme { MachinesScreen(sampleFleet(Instant.now())) }
}

@Preview(widthDp = 1280, heightDp = 800)
@Composable
private fun TabletPreview() {
    HerderTheme { MachinesScreen(sampleFleet(Instant.now())) }
}
