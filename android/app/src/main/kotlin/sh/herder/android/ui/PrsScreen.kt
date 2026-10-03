package sh.herder.android.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import sh.herder.android.CheckKind
import sh.herder.android.Grouping
import sh.herder.android.Lists
import sh.herder.android.Profile
import sh.herder.android.R
import sh.herder.android.Scope
import sh.herder.android.SessionKey
import sh.herder.android.SessionRow
import sh.herder.android.blocked
import sh.herder.android.checks
import sh.herder.android.ciMark
import sh.herder.android.driveable
import sh.herder.android.glyph
import sh.herder.android.label
import sh.herder.android.live
import sh.herder.android.ordered
import sh.herder.android.parsePrNumber
import sh.herder.android.prGroups
import sh.herder.android.prSubtitle
import sh.herder.android.word
import sh.herder.ffi.CommandBody
import sh.herder.ffi.Machine
import sh.herder.ffi.PrState
import sh.herder.ffi.PullRequest
import java.time.Instant

/** How much of a PR its row shows, as the GTK list's `Size`. */
enum class PrSize {
    /** `#12 Title` over its state, checks and head branch. */
    Full,

    /** As `Full`, without the branch: for a narrow pane. */
    Narrow,

    /** One line, as the TUI's compact strip: `#12 ✓ ! Title`. */
    Line,
}

/** Opens [url] in the browser; tests pass their own to record it. */
@Composable
fun rememberOpenUrl(override: ((String) -> Unit)?): (String) -> Unit {
    val context = LocalContext.current
    return override ?: { url ->
        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
    }
}

/** Copies [url] as the GTK row's "Copy Link" does. */
@Composable
private fun rememberCopyUrl(): (String) -> Unit {
    val context = LocalContext.current
    return { url ->
        val clipboard = context.getSystemService(ClipboardManager::class.java)
        clipboard.setPrimaryClip(ClipData.newPlainText("PR link", url))
    }
}

/**
 * The session view's list of its session's PRs, live first; it scrolls past three rows.
 * Narrow, each is one line.
 */
@Composable
fun PrStrip(
    prs: List<PullRequest>,
    compact: Boolean,
    unlink: ((ULong) -> Unit)?,
    onOpenUrl: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    if (prs.isEmpty()) return
    val copy = rememberCopyUrl()
    val size = if (compact) PrSize.Line else PrSize.Full
    Column(modifier.fillMaxWidth()) {
        Column(
            Modifier
                .fillMaxWidth()
                .heightIn(max = 200.dp)
                .verticalScroll(rememberScrollState()),
        ) {
            for (pr in ordered(prs)) {
                PrRow(pr, size, unlink?.let { { it(pr.number) } }, onOpenUrl, copy)
            }
        }
        HorizontalDivider()
    }
}

/**
 * Every session's PRs, grouped by [grouping] as the session list is: each session with PRs
 * over its rows. A session opens on its heading; a PR opens in the browser.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PrsScreen(
    profile: Profile.Open,
    grouping: Grouping,
    onGrouping: (Grouping) -> Unit,
    compact: Boolean,
    now: Instant,
    send: Sender?,
    onOpen: (SessionKey) -> Unit,
    onBack: (() -> Unit)?,
    onOpenUrl: ((String) -> Unit)? = null,
) {
    val openUrl = rememberOpenUrl(onOpenUrl)
    val copy = rememberCopyUrl()
    val snackbars = remember { SnackbarHostState() }
    val scope = rememberCoroutineScope()
    val groups = remember(profile, grouping, compact, now) {
        prGroups(Lists(profile.machines, profile.summaries, compact, now).groups(Scope.All, grouping))
    }
    val machines = profile.machines.associateBy { it.hostId }
    fun unlinkOf(row: SessionRow): ((ULong) -> Unit)? {
        val machine = machines[row.key.hostId]
        if (send == null || !driveable(machine, row.status)) return null
        return { number ->
            scope.launch {
                val error = send(row.key.hostId, CommandBody.UnlinkPr(row.key.sessionId, number))
                if (error != null) snackbars.showSnackbar(error)
            }
        }
    }
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("Pull requests")
                        Text(prSubtitle(profile.summaries), style = MaterialTheme.typography.labelMedium)
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
        snackbarHost = { SnackbarHost(snackbars) },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding), horizontalAlignment = Alignment.CenterHorizontally) {
            GroupingButtons(grouping, onGrouping)
            if (groups.isEmpty()) {
                EmptyPrs(PaddingValues())
            } else {
                val size = if (compact) PrSize.Narrow else PrSize.Full
                LazyColumn(
                    modifier = Modifier.weight(1f).widthIn(max = 840.dp).fillMaxWidth(),
                    contentPadding = PaddingValues(bottom = 16.dp),
                ) {
                    for (group in groups) {
                        item { GroupHeader(group) }
                        items(group.rows, key = { "${it.key.hostId}/${it.key.sessionId}" }) { row ->
                            SessionPrs(
                                row = row,
                                machine = machines[row.key.hostId],
                                size = size,
                                unlink = unlinkOf(row),
                                onOpen = { onOpen(row.key) },
                                onOpenUrl = openUrl,
                                onCopy = copy,
                            )
                        }
                    }
                }
            }
        }
    }
}

/** A session over its PRs, as the GTK `session_group`: title, state and place, then the rows. */
@Composable
private fun SessionPrs(
    row: SessionRow,
    machine: Machine?,
    size: PrSize,
    unlink: ((ULong) -> Unit)?,
    onOpen: () -> Unit,
    onOpenUrl: (String) -> Unit,
    onCopy: (String) -> Unit,
) {
    Column(Modifier.fillMaxWidth().padding(bottom = 8.dp)) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(12.dp))
                .clickable(onClick = onOpen)
                .heightIn(min = 48.dp)
                .padding(start = 16.dp, end = 4.dp, top = 8.dp),
        ) {
            Text(
                row.status.glyph(),
                style = MaterialTheme.typography.titleMedium,
                color = row.status.color(),
                modifier = Modifier.width(28.dp),
                textAlign = TextAlign.Center,
            )
            Column(Modifier.weight(1f)) {
                Text(row.title, style = MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
                val place = row.place ?: machine?.name
                val meta = listOfNotNull(row.status.label(), place).joinToString(" · ")
                Text(meta, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            TextButton(onClick = onOpen, modifier = Modifier.semantics { contentDescription = "Open the session" }) {
                Text("Open")
            }
        }
        for (pr in row.prs) {
            PrRow(pr, size, unlink?.let { { it(pr.number) } }, onOpenUrl, onCopy)
        }
    }
}

/** One PR as a list row. Activating it opens the PR in the browser. */
@Composable
fun PrRow(
    pr: PullRequest,
    size: PrSize,
    unlink: (() -> Unit)?,
    onOpenUrl: (String) -> Unit,
    onCopy: (String) -> Unit,
) {
    var menu by remember { mutableStateOf(false) }
    var asking by remember { mutableStateOf(false) }
    val stateColor = pr.state.color()
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .clickable { onOpenUrl(pr.url) }
            .heightIn(min = 48.dp)
            .padding(start = 16.dp, end = 4.dp)
            .semantics { contentDescription = "Open ${pr.url} in the browser" },
    ) {
        Column(Modifier.weight(1f).padding(vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    "#${pr.number}",
                    style = MaterialTheme.typography.titleSmall,
                    color = stateColor,
                    fontWeight = FontWeight.SemiBold,
                )
                if (size == PrSize.Line && live(pr)) {
                    val mark = ciMark(pr)
                    Text(mark.text, style = MaterialTheme.typography.titleSmall, color = mark.kind.color())
                    if (blocked(pr)) {
                        Text("!", style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.error)
                    }
                }
                Text(
                    pr.title,
                    style = MaterialTheme.typography.bodyLarge,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
            }
            if (size != PrSize.Line) {
                Row(
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(pr.state.word(), style = MaterialTheme.typography.labelMedium, color = stateColor)
                    for (check in checks(pr)) {
                        Text(check.text, style = MaterialTheme.typography.labelMedium, color = check.kind.color())
                    }
                    if (size == PrSize.Full) {
                        pr.headBranch?.takeIf { it.isNotEmpty() }?.let { branch ->
                            Text(
                                branch,
                                style = MaterialTheme.typography.labelMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                }
            }
        }
        Box {
            IconButton(onClick = { menu = true }) {
                Icon(painterResource(R.drawable.ic_more_vert), contentDescription = "More")
            }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(
                    text = { Text("Open in Browser") },
                    onClick = {
                        menu = false
                        onOpenUrl(pr.url)
                    },
                )
                DropdownMenuItem(
                    text = { Text("Copy Link") },
                    onClick = {
                        menu = false
                        onCopy(pr.url)
                    },
                )
                if (unlink != null) {
                    DropdownMenuItem(
                        text = { Text("Unlink…") },
                        onClick = {
                            menu = false
                            asking = true
                        },
                    )
                }
            }
        }
    }
    if (asking && unlink != null) {
        UnlinkPrDialog(pr.number, onDismiss = { asking = false }, onUnlink = unlink)
    }
}

/** Asks before unlinking PR [number], as the TUI's confirm does for destructive actions. */
@Composable
fun UnlinkPrDialog(number: ULong, onDismiss: () -> Unit, onUnlink: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Unlink #$number?") },
        text = {
            Text("herder stops tracking this pull request for the session. It can be linked again by its number.")
        },
        confirmButton = {
            TextButton(
                onClick = {
                    onUnlink()
                    onDismiss()
                },
            ) { Text("Unlink") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** Asks for a PR to link, by its number or link, and links it. */
@Composable
fun LinkPrDialog(title: String, onDismiss: () -> Unit, onLink: (ULong) -> Unit) {
    var typed by remember { mutableStateOf("") }
    val number = parsePrNumber(typed)
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Link a Pull Request") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text("To $title, by its number in the session's repository or by its link.")
                OutlinedTextField(
                    value = typed,
                    onValueChange = { typed = it },
                    singleLine = true,
                    placeholder = { Text("123, #123 or a link") },
                    shape = RoundedCornerShape(16.dp),
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = { number?.let(onLink) },
                enabled = number != null,
            ) { Text("Link") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
private fun EmptyPrs(padding: PaddingValues) {
    Column(
        modifier = Modifier.fillMaxSize().padding(padding).padding(32.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Icon(
            painterResource(R.drawable.ic_call_split),
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(48.dp),
        )
        Text("No pull requests", style = MaterialTheme.typography.titleLarge)
        Text(
            "A session's pull requests show here once it pushes its branch, or once one is linked from the session's menu.",
            style = MaterialTheme.typography.bodyMedium,
            textAlign = TextAlign.Center,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
internal fun PrState.color(): Color = when (this) {
    PrState.OPEN -> MaterialTheme.colorScheme.primary
    PrState.DRAFT -> MaterialTheme.colorScheme.onSurfaceVariant
    PrState.MERGED -> MaterialTheme.colorScheme.tertiary
    PrState.CLOSED -> MaterialTheme.colorScheme.error
}

@Composable
private fun CheckKind.color(): Color = when (this) {
    CheckKind.Ok -> MaterialTheme.colorScheme.tertiary
    CheckKind.Bad -> MaterialTheme.colorScheme.error
    CheckKind.Wait -> MaterialTheme.colorScheme.primary
    CheckKind.Unknown -> MaterialTheme.colorScheme.onSurfaceVariant
}
