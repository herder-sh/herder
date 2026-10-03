package sh.herder.android.ui

import android.content.ClipboardManager
import android.content.Context
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.withInfiniteAnimationFrameMillis
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.FilledTonalIconButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import java.io.File
import java.time.Instant
import kotlin.math.abs
import kotlin.math.roundToInt
import kotlinx.coroutines.launch
import sh.herder.android.DiffKind
import sh.herder.android.Entry
import sh.herder.android.ImageRefused
import sh.herder.android.PendingApproval
import sh.herder.android.PendingQuestion
import sh.herder.android.R
import sh.herder.android.SampleKey
import sh.herder.android.Session
import sh.herder.android.SessionKey
import sh.herder.android.Stage
import sh.herder.android.ToolApproval
import sh.herder.android.ToolKind
import sh.herder.android.command
import sh.herder.android.description
import sh.herder.android.driveable
import sh.herder.android.duration
import sh.herder.android.editLines
import sh.herder.android.firstLine
import sh.herder.android.fitsPrompt
import sh.herder.android.glyph
import sh.herder.android.imagesOn
import sh.herder.android.inWorktree
import sh.herder.android.label
import sh.herder.android.parseInput
import sh.herder.android.readImage
import sh.herder.android.sampleMachine
import sh.herder.android.sampleSession
import sh.herder.android.text
import sh.herder.android.todos
import sh.herder.android.toolGlyph
import sh.herder.android.toolKind
import sh.herder.android.toolSummary
import sh.herder.android.windowLabel
import sh.herder.ffi.Account
import sh.herder.ffi.Answer
import sh.herder.ffi.ApprovalDecision
import sh.herder.ffi.Attachment
import sh.herder.ffi.CommandBody
import sh.herder.ffi.EscalationReason
import sh.herder.ffi.HostId
import sh.herder.ffi.Image
import sh.herder.ffi.Item
import sh.herder.ffi.ItemBody
import sh.herder.ffi.Machine
import sh.herder.ffi.PermissionMode
import sh.herder.ffi.Route
import sh.herder.ffi.SessionStatus
import org.json.JSONObject

// The session view: the open session's transcript above what waits on the user or the
// composer, as docs/tui-design.md §2.1 lays out the chat and the GTK app's session view draws it.
//
// - Its pull requests sit over the transcript (docs/tui-design.md §2.4, linux/src/prs.rs):
//   number, title, branch, state, CI, review and mergeable; a tap opens the PR in the browser.
// - The transcript streams: the agent's text grows with a cursor. A tool call is one row,
//   `→ Read src/api.rs`, that expands on tap to its command, diff or output; reasoning is one
//   `+ Thought:` row that expands the same way.
// - An approval or a question replaces the composer with a card: large Allow / Deny buttons,
//   or swipe the card right to allow and left to deny; a question's choices or a typed answer.
// - The composer's chips show and switch the session's account, model and permission mode;
//   another provider's account replays the transcript there. While a turn runs a status line
//   counts its time and Stop interrupts it; prompts sent meanwhile wait, marked queued.
// - Archived and moved sessions, and a vault's, are read-only: no composer.

/** Sends a command to a machine; answers why the daemon refused it, or `null` once applied. */
typealias Sender = suspend (HostId, CommandBody) -> String?

/** Lines of an expanded tool call shown before the rest is counted. */
private const val BLOCK_LINES = 200

/** The permission modes, in the TUI's order. */
private val Modes = listOf(PermissionMode.READ_ONLY, PermissionMode.ASK, PermissionMode.AUTO_EDIT, PermissionMode.FULL_ACCESS)

/** How far the request card must be dragged to answer it. */
private val SwipeDistance = 120.dp

/**
 * The session [key], as [session] says it, of [machine]. [recent] are the models the model
 * picker offers; [send] sends the session's commands; [onOpen] opens a child session and
 * [onBack], when there is somewhere to go back to, leaves. [clock] is what time is counted from.
 * [compact] is a phone: the PR strip is one line. [onOpenUrl] opens a PR; the default uses
 * the browser. [fetchAttachment] loads a user message's image bytes; [pickImages] stands in
 * for the system photo picker in tests. [initialImages] seed the composer (screenshots).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionScreen(
    key: SessionKey,
    session: Session,
    machine: Machine?,
    recent: List<String>,
    send: Sender,
    onOpen: (SessionKey) -> Unit,
    onBack: (() -> Unit)?,
    clock: () -> Instant = Instant::now,
    compact: Boolean = false,
    onOpenUrl: ((String) -> Unit)? = null,
    fetchAttachment: (suspend (String) -> ByteArray?)? = null,
    pickImages: (() -> List<Image>)? = null,
    initialImages: List<Image> = emptyList(),
) {
    val scope = rememberCoroutineScope()
    val snackbars = remember { SnackbarHostState() }
    val openUrl = rememberOpenUrl(onOpenUrl)
    var menu by remember { mutableStateOf(false) }
    var linking by remember { mutableStateOf(false) }
    var viewing by remember { mutableStateOf<ByteArray?>(null) }
    // Prompts sent from here that are not in the transcript yet.
    val pending = remember(key) { mutableStateListOf<PendingPrompt>() }
    var seen by remember(key) { mutableIntStateOf(session.entries.size) }
    LaunchedEffect(key, session.entries.size) {
        for (entry in session.entries.drop(seen)) {
            val body = (entry as? Entry.Added)?.item?.body
            if (body is ItemBody.UserMessage) pending.removeAll { it.text == body.text }
        }
        seen = session.entries.size
    }
    val fetched = remember(key) { mutableStateMapOf<String, ByteArray?>() }
    val wanted = session.entries.flatMap { (it as? Entry.Added)?.item?.body.attachments() }
    LaunchedEffect(key, wanted.map { it.attachmentId }) {
        for (attachment in wanted) {
            val id = attachment.attachmentId
            if (id in fetched) continue
            fetched[id] = fetchAttachment?.invoke(id)
        }
    }
    // Items the user expanded, by id.
    val expanded = remember(key) { mutableStateMapOf<String, Boolean>() }
    val head = machine?.sessions?.find { it.sessionId == key.sessionId }
    val status = head?.status ?: session.status
    val readOnly = when {
        machine != null && machine.hosts.isNotEmpty() ->
            "A vault's sessions are read-only here: open the session from its own machine to drive it."
        status == SessionStatus.ARCHIVED -> "This session is archived and read-only."
        status == SessionStatus.MOVED -> "This session moved to another host and is read-only here."
        else -> null
    }
    val canLink = driveable(machine, status)
    val now = rememberNow(clock, ticking = session.running || session.approvals.isNotEmpty() || session.questions.isNotEmpty())

    fun command(body: CommandBody, prompt: String? = null, done: (Boolean) -> Unit = {}) {
        scope.launch {
            val error = send(key.hostId, body)
            if (error != null) {
                prompt?.let { text -> pending.removeAll { it.text == text } }
                snackbars.showSnackbar(error)
            }
            done(error == null)
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(
                            if (session.loaded) session.title else key.sessionId,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        val mark = stateMark(status, session)
                        val color = mark.color()
                        Text(
                            buildAnnotatedString {
                                withStyle(SpanStyle(color = color)) { append("${mark.glyph()} ${stateWord(status, session)}") }
                                listOfNotNull(machine?.name, session.branch.takeIf { it.isNotEmpty() && session.task != null })
                                    .forEach { append(" · $it") }
                            },
                            style = MaterialTheme.typography.labelMedium,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                },
                navigationIcon = {
                    if (onBack != null) {
                        IconButton(onClick = onBack) {
                            Icon(painterResource(R.drawable.ic_arrow_back), contentDescription = "Back")
                        }
                    }
                },
                actions = {
                    Box {
                        IconButton(onClick = { menu = true }) {
                            Icon(painterResource(R.drawable.ic_more_vert), contentDescription = "Session")
                        }
                        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                            DropdownMenuItem(
                                text = { Text("Link Pull Request…") },
                                enabled = canLink,
                                onClick = {
                                    menu = false
                                    linking = true
                                },
                            )
                        }
                    }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbars) },
    ) { padding ->
        Column(
            Modifier.fillMaxSize().padding(top = padding.calculateTopPadding()).imePadding(),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            PrStrip(
                prs = session.prs,
                compact = compact,
                unlink = if (canLink) {
                    { number -> command(CommandBody.UnlinkPr(key.sessionId, number)) }
                } else {
                    null
                },
                onOpenUrl = openUrl,
                modifier = Modifier.widthIn(max = 840.dp).fillMaxWidth(),
            )
            Transcript(
                session = session,
                pending = pending,
                fetched = fetched,
                expanded = expanded,
                onToggle = { id -> expanded[id] = expanded[id] != true },
                onOpen = { onOpen(SessionKey(key.hostId, it)) },
                onOpenImage = { viewing = it },
                modifier = Modifier.weight(1f).widthIn(max = 840.dp).fillMaxWidth(),
            )
            Surface(
                color = MaterialTheme.colorScheme.surfaceContainer,
                shape = RoundedCornerShape(topStart = 28.dp, topEnd = 28.dp),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Box(
                    Modifier.navigationBarsPadding().padding(horizontal = 16.dp, vertical = 12.dp),
                    contentAlignment = Alignment.TopCenter,
                ) {
                    Column(Modifier.widthIn(max = 808.dp).fillMaxWidth()) {
                        // Requests put to the user first; a user can answer the primary's too.
                        val approval = session.approvals.find { it.routedTo == Route.USER } ?: session.approvals.firstOrNull()
                        val question = session.questions.find { it.routedTo == Route.USER } ?: session.questions.firstOrNull()
                        when {
                            readOnly != null -> Text(
                                readOnly,
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.padding(vertical = 8.dp),
                            )
                            approval != null -> ApprovalCard(approval, session, now) { decision, done ->
                                command(
                                    CommandBody.AnswerApproval(key.sessionId, approval.id, decision),
                                    done = done,
                                )
                            }
                            question != null -> QuestionCard(question, session.questions.size - 1, now) { answer, done ->
                                command(CommandBody.AnswerQuestion(key.sessionId, question.id, answer), done = done)
                            }
                            else -> Composer(
                                key = key,
                                session = session,
                                accounts = machine?.accounts.orEmpty(),
                                recent = recent,
                                now = now,
                                pickImages = pickImages,
                                initialImages = initialImages,
                                onImageError = { scope.launch { snackbars.showSnackbar(it) } },
                                onPrompt = { text, images ->
                                    pending += PendingPrompt(text, images.map { it.data })
                                    command(CommandBody.SendPrompt(key.sessionId, text, images), prompt = text)
                                },
                                onStop = { command(CommandBody.Interrupt(key.sessionId)) },
                                onAccount = { account ->
                                    if (account.accountId != session.accountId) {
                                        command(
                                            if (account.provider == session.provider) {
                                                CommandBody.SwitchAccount(key.sessionId, account.accountId)
                                            } else {
                                                CommandBody.SwitchProvider(key.sessionId, account.accountId, null)
                                            },
                                        )
                                    }
                                },
                                onModel = { model ->
                                    val name = model.trim()
                                    if (name.isNotEmpty() && name != session.model) {
                                        command(CommandBody.SetModel(key.sessionId, name))
                                    }
                                },
                                onMode = { mode ->
                                    if (mode != session.permissionMode) {
                                        command(CommandBody.SetPermissionMode(key.sessionId, mode))
                                    }
                                },
                            )
                        }
                    }
                }
            }
        }
    }
    if (linking) {
        LinkPrDialog(
            title = if (session.loaded) session.title else key.sessionId,
            onDismiss = { linking = false },
            onLink = { number ->
                linking = false
                command(CommandBody.LinkPr(key.sessionId, number))
            },
        )
    }
    viewing?.let { data -> ImageViewer(data) { viewing = null } }
}

/** A prompt sent from here that the transcript has not yet taken. */
private data class PendingPrompt(val text: String, val images: List<ByteArray> = emptyList())

/** The attachments a user message carries, if it is one. */
private fun ItemBody?.attachments(): List<Attachment> = (this as? ItemBody.UserMessage)?.attachments.orEmpty()

/** The time in epoch seconds, ticking each second while [ticking]. */
@Composable
private fun rememberNow(clock: () -> Instant, ticking: Boolean): Long {
    var now by remember { mutableLongStateOf(clock().epochSecond) }
    if (ticking) {
        LaunchedEffect(Unit) {
            // An infinite operation, which tests do not run.
            while (true) {
                withInfiniteAnimationFrameMillis {}
                val second = clock().epochSecond
                if (second != now) now = second
            }
        }
    }
    return now
}

/** A row of the transcript. */
private sealed interface TranscriptRow {
    data class Done(val entry: Entry) : TranscriptRow

    data class Streaming(val item: Item) : TranscriptRow

    data class Queued(val prompt: PendingPrompt) : TranscriptRow
}

/** Whether an entry draws a row: a tool's result is drawn with its call. */
private fun Entry.shown(): Boolean {
    val body = (this as? Entry.Added)?.item?.body ?: return true
    return body !is ItemBody.ToolResult && body !is ItemBody.Unknown
}

/** The transcript, newest at the bottom, which it stays on while it is scrolled there. */
@Composable
private fun Transcript(
    session: Session,
    pending: List<PendingPrompt>,
    fetched: Map<String, ByteArray?>,
    expanded: Map<String, Boolean>,
    onToggle: (String) -> Unit,
    onOpen: (String) -> Unit,
    onOpenImage: (ByteArray) -> Unit,
    modifier: Modifier,
) {
    val rows = buildList {
        session.entries.filter { it.shown() }.forEach { add(TranscriptRow.Done(it)) }
        session.streaming.forEach { add(TranscriptRow.Streaming(it)) }
        pending.forEach { add(TranscriptRow.Queued(it)) }
    }
    if (rows.isEmpty()) {
        Box(modifier.padding(32.dp), contentAlignment = Alignment.Center) {
            Text(
                if (session.loaded) "Nothing here yet. Write a prompt to start." else "Loading the session…",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
        }
        return
    }
    // Reversed, the newest row is the first: a new row or growing text keeps the bottom in view.
    LazyColumn(
        modifier = modifier,
        reverseLayout = true,
        contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 16.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp, Alignment.Bottom),
    ) {
        items(rows.asReversed()) { row ->
            when (row) {
                is TranscriptRow.Done -> EntryRow(row.entry, session, fetched, expanded, onToggle, onOpen, onOpenImage)
                is TranscriptRow.Streaming -> ItemRow(row.item, session, streaming = true, fetched, expanded, onToggle, onOpenImage)
                is TranscriptRow.Queued -> UserMessage(
                    row.prompt.text,
                    queued = true,
                    images = row.prompt.images.map { ShownImage.Ready(it) },
                    onOpenImage = onOpenImage,
                )
            }
        }
    }
}

@Composable
private fun EntryRow(
    entry: Entry,
    session: Session,
    fetched: Map<String, ByteArray?>,
    expanded: Map<String, Boolean>,
    onToggle: (String) -> Unit,
    onOpen: (String) -> Unit,
    onOpenImage: (ByteArray) -> Unit,
) {
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    when (entry) {
        is Entry.Added -> ItemRow(entry.item, session, streaming = false, fetched, expanded, onToggle, onOpenImage)
        is Entry.Notice -> Line(
            entry.text,
            if (entry.attention) MaterialTheme.colorScheme.primary else muted,
            weight = if (entry.attention) FontWeight.SemiBold else null,
        )
        is Entry.TurnEnded -> {
            val accent = MaterialTheme.colorScheme.primary
            val parts = listOf(entry.account, entry.model).filter { it.isNotEmpty() } +
                listOfNotNull(entry.took?.let(::duration), "interrupted".takeIf { entry.interrupted })
            Text(
                buildAnnotatedString {
                    withStyle(SpanStyle(color = accent)) { append("▣ ") }
                    append(parts.joinToString(" · "))
                },
                style = MaterialTheme.typography.labelMedium,
                color = muted,
                modifier = Modifier.padding(top = 2.dp, bottom = 10.dp),
            )
        }
        is Entry.TurnFailed -> Barred(MaterialTheme.colorScheme.error) {
            Text(
                "✗ ${entry.errorClass.label()}",
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.error,
            )
            Text(entry.message, style = MaterialTheme.typography.bodyMedium)
        }
        is Entry.Switch -> Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp),
        ) {
            HorizontalDivider(Modifier.weight(1f))
            Text(
                entry.text,
                style = MaterialTheme.typography.labelMedium,
                color = muted,
                textAlign = TextAlign.Center,
                // Measured first: the rules take what the text leaves.
                modifier = Modifier.padding(horizontal = 12.dp),
            )
            HorizontalDivider(Modifier.weight(1f))
        }
        is Entry.Resolved -> Line("${if (entry.approval) "△" else "?"} ${entry.text}", muted)
        is Entry.Child -> Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .rounded()
                .clickable { onOpen(entry.sessionId) }
                .heightIn(min = 44.dp),
        ) {
            Glyph("◇", MaterialTheme.colorScheme.primary)
            Text(
                buildAnnotatedString {
                    withStyle(SpanStyle(fontWeight = FontWeight.SemiBold)) { append("Task ") }
                    append(entry.task)
                },
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Text("Open", style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary)
            Spacer(Modifier.width(8.dp))
        }
        is Entry.Report -> Line("↳ report: ${firstLine(entry.summary)}", muted)
        is Entry.Pr -> {
            val pr = session.prs.find { it.number == entry.number }
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.heightIn(min = 32.dp)) {
                Glyph("⎇", muted)
                if (pr == null) {
                    Text("#${entry.number} unlinked", style = MaterialTheme.typography.bodyMedium, color = muted)
                } else {
                    PrPill(pr)
                    Spacer(Modifier.width(8.dp))
                    Text(pr.title, style = MaterialTheme.typography.bodyMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
            }
        }
    }
}

@Composable
private fun ItemRow(
    item: Item,
    session: Session,
    streaming: Boolean,
    fetched: Map<String, ByteArray?>,
    expanded: Map<String, Boolean>,
    onToggle: (String) -> Unit,
    onOpenImage: (ByteArray) -> Unit,
) {
    when (val body = item.body) {
        is ItemBody.UserMessage -> UserMessage(
            body.text,
            queued = false,
            images = body.attachments.map { attachment ->
                fetched[attachment.attachmentId]?.let { ShownImage.Ready(it) } ?: ShownImage.Missing
            },
            onOpenImage = onOpenImage,
        )
        is ItemBody.AssistantMessage -> Markdown(body.text, cursor = streaming, modifier = Modifier.padding(vertical = 6.dp))
        is ItemBody.Reasoning -> {
            val open = expanded[item.id] == true
            Column(
                Modifier.fillMaxWidth().rounded().clickable { onToggle(item.id) }.padding(vertical = 6.dp),
            ) {
                Text(
                    "+ Thought: ${firstLine(body.text)}",
                    style = MaterialTheme.typography.bodyMedium.copy(fontStyle = FontStyle.Italic),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (open) {
                    Text(
                        body.text,
                        style = MaterialTheme.typography.bodyMedium.copy(fontStyle = FontStyle.Italic),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(start = 16.dp, top = 4.dp),
                    )
                }
            }
        }
        is ItemBody.ToolCall -> ToolCall(item.id, body, session, streaming, expanded[item.id] == true) { onToggle(item.id) }
        is ItemBody.ToolResult, ItemBody.Unknown -> {}
    }
}

/**
 * The user's message: on a card, behind a bar in the accent, with its images as thumbnails;
 * [queued] while it waits.
 */
@Composable
private fun UserMessage(
    text: String,
    queued: Boolean,
    images: List<ShownImage> = emptyList(),
    onOpenImage: (ByteArray) -> Unit = {},
) {
    Barred(MaterialTheme.colorScheme.primary, Modifier.padding(vertical = 6.dp)) {
        if (text.isNotEmpty()) {
            Text(text, style = MaterialTheme.typography.bodyLarge)
        }
        MessageImages(images, onOpenImage)
        if (queued) {
            Text(
                "QUEUED",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSecondaryContainer,
                modifier = Modifier
                    .padding(top = 6.dp)
                    .background(MaterialTheme.colorScheme.secondaryContainer, RoundedCornerShape(6.dp))
                    .padding(horizontal = 6.dp, vertical = 2.dp),
            )
        }
    }
}

/** A card behind a bar in [bar]. */
@Composable
private fun Barred(bar: Color, modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerHigh,
        shape = RoundedCornerShape(topEnd = 16.dp, bottomEnd = 16.dp, topStart = 4.dp, bottomStart = 4.dp),
        modifier = modifier.fillMaxWidth(),
    ) {
        Row(Modifier.height(IntrinsicSize.Min)) {
            Box(Modifier.width(4.dp).fillMaxHeight().background(bar))
            Column(Modifier.padding(horizontal = 14.dp, vertical = 12.dp)) { content() }
        }
    }
}

/** A tappable row's ripple shape. */
private fun Modifier.rounded(): Modifier = clip(RoundedCornerShape(12.dp))

/** A muted one-line entry. */
@Composable
private fun Line(text: String, color: Color, weight: FontWeight? = null) {
    Text(
        text,
        style = MaterialTheme.typography.bodyMedium,
        color = color,
        fontWeight = weight,
        modifier = Modifier.padding(vertical = 4.dp),
    )
}

/** A row's leading glyph, in a fixed column. */
@Composable
private fun Glyph(glyph: String, color: Color) {
    Text(
        glyph,
        style = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
        color = color,
        textAlign = TextAlign.Center,
        modifier = Modifier.width(28.dp),
    )
}

/**
 * A tool call: one row, glyph and summary, in `primary` while it waits on an approval, struck
 * through when denied and in `error` when it failed; a tap shows what it ran and its output.
 */
@Composable
private fun ToolCall(
    id: String,
    call: ItemBody.ToolCall,
    session: Session,
    streaming: Boolean,
    open: Boolean,
    onToggle: () -> Unit,
) {
    val result = session.result(id)
    val approval = session.toolApprovals[id]
    val (label, args) = remember(call, result) { toolSummary(call.name, call.input, result?.output, session.worktree) }
    val lines = remember(call, result, session.worktree) { block(call, result, session.worktree) }
    val color = when {
        approval == ToolApproval.Pending -> MaterialTheme.colorScheme.primary
        result?.isError == true -> MaterialTheme.colorScheme.error
        approval == ToolApproval.Denied || (streaming && result == null) -> MaterialTheme.colorScheme.onSurfaceVariant
        else -> MaterialTheme.colorScheme.onSurface
    }
    val expandable = lines.isNotEmpty()
    Column(Modifier.fillMaxWidth()) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .rounded()
                .then(if (expandable) Modifier.clickable(onClick = onToggle) else Modifier)
                .heightIn(min = 40.dp),
        ) {
            Glyph(if (streaming && result == null) "~" else toolGlyph(call.name), color)
            Text(
                buildAnnotatedString {
                    if (label.isNotEmpty()) {
                        withStyle(SpanStyle(fontWeight = FontWeight.SemiBold)) { append(label) }
                        append(" ")
                    }
                    append(args)
                    if (streaming && result == null) append("…")
                },
                style = MaterialTheme.typography.bodyMedium.copy(
                    textDecoration = if (approval == ToolApproval.Denied) TextDecoration.LineThrough else null,
                ),
                color = color,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            if (expandable) {
                Icon(
                    painterResource(if (open) R.drawable.ic_expand_less else R.drawable.ic_expand_more),
                    contentDescription = if (open) "Collapse" else "Expand",
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.size(20.dp),
                )
                Spacer(Modifier.width(4.dp))
            }
        }
        if (open && expandable) ToolBlock(lines)
    }
}

/** A line of a tool's expanded block, and how it is coloured. */
private data class BlockLine(val text: String, val kind: BlockKind)

private enum class BlockKind { Command, Plain, Added, Removed, Error }

/** What a tool call's block shows: its command or diff or content, then its output. */
private fun block(call: ItemBody.ToolCall, result: ItemBody.ToolResult?, worktree: String): List<BlockLine> {
    val input = parseInput(call.input)
    val lines = mutableListOf<BlockLine>()
    when (toolKind(call.name)) {
        ToolKind.Shell -> command(input).takeIf { it.isNotEmpty() }?.let {
            lines += inWorktree(it, worktree).lines().map { line -> BlockLine(line, BlockKind.Command) }
        }
        ToolKind.Edit -> editLines(call.name, input).forEach {
            lines += when (it.kind) {
                DiffKind.Added -> BlockLine("+ ${it.text}", BlockKind.Added)
                DiffKind.Removed -> BlockLine("− ${it.text}", BlockKind.Removed)
                DiffKind.Context -> BlockLine("  ${it.text}", BlockKind.Plain)
            }
        }
        ToolKind.Write -> ((input as? JSONObject)?.opt("content") as? String)?.let { content ->
            lines += content.lines().mapIndexed { n, line -> BlockLine("${n + 1}  $line", BlockKind.Plain) }
        }
        ToolKind.Todo -> todos(input).forEach { (text, status) ->
            val mark = when (status) {
                "completed" -> "[✓]"
                "in_progress" -> "[•]"
                else -> "[ ]"
            }
            lines += BlockLine("$mark $text", BlockKind.Plain)
        }
        else -> {}
    }
    val output = result?.output?.trimEnd().orEmpty()
    // An edit's or a write's output only says it is done.
    val quiet = toolKind(call.name) in setOf(ToolKind.Edit, ToolKind.Write, ToolKind.Todo) && result?.isError != true
    if (output.isNotEmpty() && !quiet) {
        val kind = if (result?.isError == true) BlockKind.Error else BlockKind.Plain
        lines += output.lines().map { BlockLine(it, kind) }
    }
    return lines
}

@Composable
private fun ToolBlock(lines: List<BlockLine>) {
    val shown = lines.take(BLOCK_LINES)
    val style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace)
    val colors = MaterialTheme.colorScheme
    Surface(
        color = colors.surfaceContainerHigh,
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier.fillMaxWidth().padding(start = 28.dp, top = 2.dp, bottom = 6.dp),
    ) {
        Column(Modifier.horizontalScroll(rememberScrollState()).padding(12.dp)) {
            for (line in shown) {
                Text(
                    if (line.kind == BlockKind.Command) "$ ${line.text}" else line.text,
                    style = style,
                    softWrap = false,
                    fontWeight = if (line.kind == BlockKind.Command) FontWeight.SemiBold else null,
                    color = when (line.kind) {
                        BlockKind.Added -> colors.tertiary
                        BlockKind.Removed, BlockKind.Error -> colors.error
                        BlockKind.Command, BlockKind.Plain -> colors.onSurface
                    },
                )
            }
            if (lines.size > shown.size) {
                Text("… ${lines.size - shown.size} more lines", style = style, color = colors.onSurfaceVariant)
            }
        }
    }
}

/** What waits on the user marks a running session as needing them, as the TUI's §3.2 does. */
private fun stateMark(status: SessionStatus, session: Session): SessionStatus {
    val waitingOnYou = session.approvals.any { it.routedTo == Route.USER } || session.questions.any { it.routedTo == Route.USER }
    return if (waitingOnYou && status == SessionStatus.RUNNING) SessionStatus.NEEDS_YOU else status
}

private fun stateWord(status: SessionStatus, session: Session): String = stateMark(status, session).label()

/** How long ago [since] was, as a request's header says it. */
private fun age(since: Long?, now: Long): String? = since?.let { "asked ${duration(now - it)} ago" }

/**
 * A card that answers when swiped: right far enough calls [onRight], left [onLeft]. What each
 * does shows behind the card as it moves.
 */
@Composable
private fun Swipeable(
    enabled: Boolean,
    right: String,
    left: String,
    onRight: () -> Unit,
    onLeft: () -> Unit,
    content: @Composable () -> Unit,
) {
    val offset = remember { Animatable(0f) }
    val scope = rememberCoroutineScope()
    val distance = with(LocalDensity.current) { SwipeDistance.toPx() }
    val colors = MaterialTheme.colorScheme
    Box(Modifier.fillMaxWidth()) {
        if (offset.value != 0f) {
            val toRight = offset.value > 0
            Row(
                Modifier
                    .matchParentSize()
                    .background(if (toRight) colors.primaryContainer else colors.errorContainer, RoundedCornerShape(24.dp))
                    .padding(horizontal = 24.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = if (toRight) Arrangement.Start else Arrangement.End,
            ) {
                Text(
                    if (toRight) right else left,
                    style = MaterialTheme.typography.titleMedium,
                    color = if (toRight) colors.onPrimaryContainer else colors.onErrorContainer,
                )
            }
        }
        Box(
            Modifier
                .offset { IntOffset(offset.value.roundToInt(), 0) }
                .pointerInput(enabled) {
                    if (!enabled) return@pointerInput
                    detectHorizontalDragGestures(
                        onDragEnd = {
                            val value = offset.value
                            scope.launch { offset.animateTo(0f) }
                            if (abs(value) >= distance) if (value > 0) onRight() else onLeft()
                        },
                        onDragCancel = { scope.launch { offset.animateTo(0f) } },
                        onHorizontalDrag = { change, amount ->
                            change.consume()
                            scope.launch { offset.snapTo(offset.value + amount) }
                        },
                    )
                },
        ) { content() }
    }
}

/** A request card's frame: its kind, what it is about, its age and how many more wait. */
@Composable
private fun RequestCard(kind: String, about: String, age: String?, more: Int, content: @Composable () -> Unit) {
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerHighest,
        shape = RoundedCornerShape(24.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                val accent = MaterialTheme.colorScheme.primary
                val muted = MaterialTheme.colorScheme.onSurfaceVariant
                Text(
                    buildAnnotatedString {
                        withStyle(SpanStyle(color = accent, fontWeight = FontWeight.SemiBold)) {
                            append(kind)
                        }
                        if (about.isNotEmpty()) {
                            withStyle(SpanStyle(color = muted)) { append(" · ") }
                            withStyle(SpanStyle(fontWeight = FontWeight.SemiBold)) { append(about) }
                        }
                    },
                    style = MaterialTheme.typography.titleSmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                val meta = listOfNotNull(age, "+$more more".takeIf { more > 0 }).joinToString(" · ")
                if (meta.isNotEmpty()) {
                    Text(meta, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            content()
        }
    }
}

/** Why a request is the user's, or that it went to the primary session first. */
@Composable
private fun Why(routedTo: Route, reason: EscalationReason?, note: String?) {
    val lines = buildList {
        if (routedTo == Route.PRIMARY) add("Asked the primary session first; you can answer too.")
        else reason?.let { add("Escalated: ${it.text()}.") }
        note?.let { add("The primary says: ${firstLine(it)}") }
    }
    for (line in lines) {
        Text(line, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/** A request's command or diff, monospace, scrolling past about 15 lines. */
@Composable
private fun CommandView(command: String) {
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerLow,
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(
            command,
            style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
            modifier = Modifier.heightIn(max = 260.dp).verticalScroll(rememberScrollState()).padding(12.dp),
        )
    }
}

/** An approval: what the tool would do, and Allow / Deny, as buttons or a swipe. */
@Composable
private fun ApprovalCard(
    approval: PendingApproval,
    session: Session,
    now: Long,
    answer: (ApprovalDecision, (Boolean) -> Unit) -> Unit,
) {
    var answering by remember(approval.id) { mutableStateOf(false) }
    val call = session.toolCall(approval.toolCallId)
    val tool = call?.name ?: "tool"
    val shown = remember(call, session.worktree) {
        call?.let {
            if (toolKind(it.name) == ToolKind.Shell) {
                "$ " + inWorktree(command(parseInput(it.input)), session.worktree)
            } else {
                val (label, args) = toolSummary(it.name, it.input, null, session.worktree)
                "$label $args".trim()
            }
        }?.takeIf { it.isNotEmpty() && it != "$" } ?: approval.summary
    }
    fun decide(decision: ApprovalDecision) {
        if (answering) return
        answering = true
        answer(decision) { applied -> if (!applied) answering = false }
    }
    Swipeable(
        enabled = !answering,
        right = "Allow",
        left = "Deny",
        onRight = { decide(ApprovalDecision.ALLOW) },
        onLeft = { decide(ApprovalDecision.DENY) },
    ) {
        RequestCard("△ approval", tool, age(approval.since, now), session.approvals.size - 1) {
            if (approval.summary.isNotEmpty() && approval.summary != shown) {
                Text(approval.summary, style = MaterialTheme.typography.bodyMedium)
            }
            CommandView(shown)
            Why(approval.routedTo, approval.reason, approval.note)
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                OutlinedButton(
                    onClick = { decide(ApprovalDecision.DENY) },
                    enabled = !answering,
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                    modifier = Modifier.weight(1f).height(56.dp),
                ) { Text("Deny", style = MaterialTheme.typography.titleMedium) }
                Button(
                    onClick = { decide(ApprovalDecision.ALLOW) },
                    enabled = !answering,
                    modifier = Modifier.weight(1f).height(56.dp),
                ) { Text("Allow", style = MaterialTheme.typography.titleMedium) }
            }
            Text(
                "Swipe right to allow, left to deny",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

/** A question: its choices, each a large button, or a typed answer. */
@Composable
private fun QuestionCard(
    question: PendingQuestion,
    more: Int,
    now: Long,
    answer: (Answer, (Boolean) -> Unit) -> Unit,
) {
    var answering by remember(question.id) { mutableStateOf(false) }
    var typed by rememberSaveable(question.id) { mutableStateOf("") }
    fun reply(value: Answer) {
        if (answering) return
        answering = true
        answer(value) { applied -> if (!applied) answering = false }
    }
    RequestCard("? question", "", age(question.since, now), more) {
        Markdown(question.text)
        Why(question.routedTo, question.reason, question.note)
        question.choices.forEachIndexed { index, choice ->
            OutlinedButton(
                onClick = { reply(Answer.Choice(index.toUInt())) },
                enabled = !answering,
                contentPadding = PaddingValues(horizontal = 16.dp, vertical = 12.dp),
                modifier = Modifier.fillMaxWidth().heightIn(min = 56.dp),
            ) {
                Text(
                    "${index + 1}",
                    style = MaterialTheme.typography.titleMedium,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.width(28.dp),
                )
                Text(
                    choice,
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurface,
                    modifier = Modifier.weight(1f),
                )
            }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = typed,
                onValueChange = { typed = it },
                placeholder = { Text(if (question.choices.isEmpty()) "Type an answer" else "Or type an answer") },
                enabled = !answering,
                shape = RoundedCornerShape(16.dp),
                modifier = Modifier.weight(1f),
            )
            Spacer(Modifier.width(8.dp))
            FilledIconButton(
                onClick = { reply(Answer.Text(typed.trim())) },
                enabled = !answering && typed.isNotBlank(),
                modifier = Modifier.size(48.dp),
            ) { Icon(painterResource(R.drawable.ic_send), contentDescription = "Answer") }
        }
    }
}

/** Reads [uri] into the prompt's images, or says why it could not. */
private fun attachUri(
    context: Context,
    uri: Uri,
    current: List<Image>,
    onError: (String) -> Unit,
    set: (List<Image>) -> Unit,
) {
    val image = try {
        readImage(context, uri)
    } catch (error: ImageRefused) {
        onError(error.message)
        return
    }
    if (!fitsPrompt(current, image)) {
        onError("Those images are too large to send together.")
        return
    }
    set(current + image)
}

/** Which of the composer's pickers is open. */
private enum class Picker { Account, Model, Mode }

/**
 * The composer: the prompt, sent with its button, and Stop while a turn runs; above it the
 * session's account, model and mode, each a chip that opens its picker. Photos, the camera
 * and a paste attach images as removable thumbnails over the field.
 */
@Composable
private fun Composer(
    key: SessionKey,
    session: Session,
    accounts: List<Account>,
    recent: List<String>,
    now: Long,
    pickImages: (() -> List<Image>)?,
    initialImages: List<Image>,
    onImageError: (String) -> Unit,
    onPrompt: (String, List<Image>) -> Unit,
    onStop: () -> Unit,
    onAccount: (Account) -> Unit,
    onModel: (String) -> Unit,
    onMode: (PermissionMode) -> Unit,
) {
    var text by rememberSaveable(key) { mutableStateOf("") }
    var images by remember(key) { mutableStateOf(initialImages) }
    var picker by remember { mutableStateOf<Picker?>(null) }
    val context = LocalContext.current
    val pickPhoto = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        uri?.let { attachUri(context, it, images, onImageError) { images = it } }
    }
    var captureUri by remember { mutableStateOf<Uri?>(null) }
    val takePicture = rememberLauncherForActivityResult(ActivityResultContracts.TakePicture()) { ok ->
        val uri = captureUri
        if (ok && uri != null) attachUri(context, uri, images, onImageError) { images = it }
    }
    val account = accounts.find { it.accountId == session.accountId }
    // The account's busiest window, as the TUI's status line shows it.
    val usage = account?.usage?.maxByOrNull { it.usedPercent }
        ?.let { "${account.accountId} ${windowLabel(it.window)} ${it.usedPercent.roundToInt()}%" }
    fun attach(added: List<Image>) {
        var next = images
        for (image in added) {
            if (!fitsPrompt(next, image)) {
                onImageError("Those images are too large to send together.")
                break
            }
            next = next + image
        }
        images = next
    }

    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        PendingStrip(images.map { it.data }) { index ->
            images = images.filterIndexed { n, _ -> n != index }
        }
        if (session.running || usage != null) {
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(horizontal = 4.dp)) {
                if (session.running) {
                    CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.width(8.dp))
                    val took = session.turnStarted?.let { " · ${duration(now - it)}" }.orEmpty()
                    Text("working$took", style = MaterialTheme.typography.labelMedium)
                }
                Spacer(Modifier.weight(1f))
                usage?.let {
                    Text(it, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.horizontalScroll(rememberScrollState()),
        ) {
            fun toggle(it: Picker) {
                picker = if (picker == it) null else it
            }
            Chip(session.accountId ?: "account", "Account") { toggle(Picker.Account) }
            Chip(session.model.ifEmpty { "default model" }, "Model") { toggle(Picker.Model) }
            Chip(session.permissionMode.label(), "Mode") { toggle(Picker.Mode) }
        }
        // A picker takes the prompt's place, in reach of a thumb.
        when (picker) {
            Picker.Account -> AccountPicker(session, accounts, onDismiss = { picker = null }) {
                picker = null
                onAccount(it)
            }
            Picker.Model -> ModelPicker(session.model, recent, onDismiss = { picker = null }) {
                picker = null
                onModel(it)
            }
            Picker.Mode -> Panel(onDismiss = { picker = null }) {
                for (mode in Modes) {
                    PickerRow(mode.label(), mode.description(), null, mode == session.permissionMode) {
                        picker = null
                        onMode(mode)
                    }
                }
            }
            null -> Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    IconButton(
                        onClick = {
                            if (pickImages != null) attach(pickImages())
                            else pickPhoto.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
                        },
                    ) {
                        Icon(painterResource(R.drawable.ic_photo), contentDescription = "Attach photo")
                    }
                    IconButton(
                        onClick = {
                            val file = File(context.cacheDir, "captures/${System.currentTimeMillis()}.jpg")
                            file.parentFile?.mkdirs()
                            val uri = FileProvider.getUriForFile(context, "${context.packageName}.images", file)
                            captureUri = uri
                            takePicture.launch(uri)
                        },
                    ) {
                        Icon(painterResource(R.drawable.ic_camera), contentDescription = "Take photo")
                    }
                    IconButton(
                        onClick = {
                            val clipboard = context.getSystemService(ClipboardManager::class.java)
                            val pasted = imagesOn(context, clipboard?.primaryClip)
                            if (pasted.isEmpty()) onImageError("The clipboard has no image.")
                            else attach(pasted)
                        },
                    ) {
                        Icon(painterResource(R.drawable.ic_image), contentDescription = "Paste image")
                    }
                }
                Row(verticalAlignment = Alignment.Bottom) {
                    TextField(
                        value = text,
                        onValueChange = { text = it },
                        placeholder = { Text(if (session.running) "Queue a prompt for after this turn" else "Write a prompt") },
                        maxLines = 6,
                        shape = RoundedCornerShape(24.dp),
                        colors = TextFieldDefaults.colors(
                            focusedIndicatorColor = Color.Transparent,
                            unfocusedIndicatorColor = Color.Transparent,
                            disabledIndicatorColor = Color.Transparent,
                        ),
                        modifier = Modifier.weight(1f),
                    )
                    if (session.running) {
                        Spacer(Modifier.width(8.dp))
                        FilledTonalIconButton(onClick = onStop, modifier = Modifier.size(56.dp)) {
                            Icon(painterResource(R.drawable.ic_stop), contentDescription = "Stop")
                        }
                    }
                    Spacer(Modifier.width(8.dp))
                    FilledIconButton(
                        onClick = {
                            val prompt = text.trim()
                            if (prompt.isNotEmpty() || images.isNotEmpty()) {
                                onPrompt(prompt, images)
                                text = ""
                                images = emptyList()
                            }
                        },
                        enabled = text.isNotBlank() || images.isNotEmpty(),
                        modifier = Modifier.size(56.dp),
                    ) { Icon(painterResource(R.drawable.ic_send), contentDescription = "Send") }
                }
            }
        }
    }
}

/** A composer control: what it is set to, and an arrow for its picker. */
@Composable
private fun Chip(value: String, what: String, onClick: () -> Unit) {
    AssistChip(
        onClick = onClick,
        label = { Text(value, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.widthIn(max = 180.dp)) },
        trailingIcon = { Icon(painterResource(R.drawable.ic_arrow_drop_down), contentDescription = null, Modifier.size(18.dp)) },
        modifier = Modifier.semantics { contentDescription = "$what: $value" },
    )
}

@Composable
private fun Check(shown: Boolean) {
    if (shown) {
        Icon(painterResource(R.drawable.ic_check), contentDescription = "current", tint = MaterialTheme.colorScheme.primary)
    } else {
        Spacer(Modifier.size(24.dp))
    }
}

/** A picker's rows in place of the prompt, and Cancel. */
@Composable
private fun Panel(onDismiss: () -> Unit, content: @Composable () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        content()
        Row(horizontalArrangement = Arrangement.End, modifier = Modifier.fillMaxWidth()) {
            TextButton(onClick = onDismiss) { Text("Cancel") }
        }
    }
}

/** A choice of a picker: what it is, what it means or is, a [detail] and whether it is [current]. */
@Composable
private fun PickerRow(title: String, subtitle: String, detail: String?, current: Boolean, onClick: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .rounded()
            .clickable(onClick = onClick)
            .heightIn(min = 56.dp)
            .padding(horizontal = 12.dp, vertical = 6.dp),
    ) {
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            Text(subtitle, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        detail?.let {
            Text(it, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Spacer(Modifier.width(12.dp))
        }
        Check(current)
    }
}

/**
 * The accounts to switch to: the same provider's, where the conversation continues, and other
 * providers', which replay the transcript; each with its busiest window.
 */
@Composable
private fun AccountPicker(session: Session, accounts: List<Account>, onDismiss: () -> Unit, onPick: (Account) -> Unit) {
    Panel(onDismiss) {
        if (accounts.isEmpty()) {
            Text(
                "This machine lists no accounts.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(12.dp),
            )
        }
        val (same, other) = accounts.partition { it.provider == session.provider }
        for ((heading, group) in listOf(
            "Same provider · the conversation continues" to same,
            "Other provider · replays the transcript" to other,
        )) {
            if (group.isEmpty()) continue
            Text(
                heading,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.padding(start = 12.dp, end = 12.dp, top = 8.dp, bottom = 2.dp),
            )
            for (account in group) {
                val window = account.usage.maxByOrNull { it.usedPercent }
                PickerRow(
                    title = account.label.ifEmpty { account.accountId },
                    subtitle = "${account.accountId} · ${account.provider}",
                    detail = window?.let { "${windowLabel(it.window)} ${it.usedPercent.roundToInt()}%" },
                    current = account.accountId == session.accountId,
                ) { onPick(account) }
            }
        }
    }
}

/**
 * The model, in place of the prompt: typed, or one of the models the app's sessions of this
 * provider use.
 */
@Composable
private fun ModelPicker(current: String, recent: List<String>, onDismiss: () -> Unit, onPick: (String) -> Unit) {
    var typed by remember { mutableStateOf(current) }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedTextField(
            value = typed,
            onValueChange = { typed = it },
            singleLine = true,
            label = { Text("Model") },
            shape = RoundedCornerShape(16.dp),
            modifier = Modifier.fillMaxWidth(),
        )
        if (recent.isNotEmpty()) {
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.horizontalScroll(rememberScrollState()),
            ) {
                Text(
                    "Used by your sessions",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                for (model in recent) {
                    AssistChip(
                        onClick = { onPick(model) },
                        label = { Text(model) },
                        leadingIcon = if (model == current) {
                            { Icon(painterResource(R.drawable.ic_check), contentDescription = "current", Modifier.size(18.dp)) }
                        } else {
                            null
                        },
                    )
                }
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End), modifier = Modifier.fillMaxWidth()) {
            TextButton(onClick = onDismiss) { Text("Cancel") }
            Button(onClick = { onPick(typed) }, enabled = typed.isNotBlank()) { Text("Switch") }
        }
    }
}

@Preview(widthDp = 411, heightDp = 891)
@Composable
private fun ApprovalPreview() {
    HerderTheme {
        SessionScreen(SampleKey, sampleSession(Stage.Approval), sampleMachine(), listOf("opus"), { _, _ -> null }, {}, {})
    }
}

@Preview(widthDp = 411, heightDp = 891)
@Composable
private fun ChatPreview() {
    HerderTheme {
        SessionScreen(SampleKey, sampleSession(Stage.Chat), sampleMachine(), listOf("opus"), { _, _ -> null }, {}, {})
    }
}
