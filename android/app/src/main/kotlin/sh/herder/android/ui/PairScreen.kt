package sh.herder.android.ui

import android.Manifest
import android.content.ClipboardManager
import android.content.Context
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import kotlinx.coroutines.launch
import sh.herder.android.ParsedPairing
import sh.herder.android.R
import sh.herder.android.SAMPLE_PAIR_FP
import sh.herder.android.SAMPLE_PAIR_LINK
import sh.herder.android.groupedFingerprint
import sh.herder.android.machine
import sh.herder.android.pairingLink
import sh.herder.android.parsePairing
import sh.herder.ffi.ConnectionState
import sh.herder.ffi.Machine

/**
 * Pairs with a machine from the `herder://pair` link `herder pair` prints: scan the QR
 * code or paste the link, confirm the fingerprint, then [onPair]. [scannerPreview] skips
 * CameraX so tests and screenshots can show the scanner chrome.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PairScreen(
    machines: List<Machine> = emptyList(),
    initialLink: String = "",
    onPair: (suspend (String) -> String?)? = null,
    onClose: () -> Unit,
    startScanning: Boolean = false,
    scannerPreview: Boolean = false,
) {
    var link by rememberSaveable { mutableStateOf(initialLink) }
    LaunchedEffect(initialLink) {
        if (initialLink.isNotEmpty()) link = initialLink
    }
    var error by remember { mutableStateOf<String?>(null) }
    var pairing by remember { mutableStateOf(false) }
    var scanning by remember { mutableStateOf(startScanning) }
    val found = pairingLink(link)
    val uri = found?.let(::parsePairing)
    val scope = rememberCoroutineScope()
    val scroll = rememberScrollState()
    val context = LocalContext.current
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) {
            scanning = true
        } else {
            error = "herder needs the camera to scan the code. Or paste the link instead."
        }
    }

    fun startScan() {
        error = null
        if (scannerPreview ||
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
            PackageManager.PERMISSION_GRANTED
        ) {
            scanning = true
        } else {
            permission.launch(Manifest.permission.CAMERA)
        }
    }

    fun pair() {
        val toPair = found ?: return
        if (pairing) return
        pairing = true
        error = null
        scope.launch {
            val failed = onPair?.invoke(toPair)
            pairing = false
            if (failed == null) onClose() else error = failed
        }
    }

    if (scanning) {
        PairScanner(
            onScanned = { scanned ->
                link = scanned
                scanning = false
                error = null
            },
            onClose = { scanning = false },
            preview = scannerPreview,
        )
        return@PairScreen
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("Add machine")
                        Text(
                            "Pair this device with a machine running herder.",
                            style = MaterialTheme.typography.labelMedium,
                        )
                    }
                },
                navigationIcon = {
                    IconButton(onClick = onClose) {
                        Icon(painterResource(R.drawable.ic_arrow_back), contentDescription = "Back")
                    }
                },
            )
        },
        bottomBar = {
            Surface(tonalElevation = 3.dp) {
                Button(
                    onClick = ::pair,
                    enabled = uri != null && !pairing,
                    modifier = Modifier
                        .navigationBarsPadding()
                        .imePadding()
                        .fillMaxWidth()
                        .padding(horizontal = 20.dp, vertical = 12.dp)
                        .semantics { contentDescription = "Pair" },
                ) {
                    if (pairing) {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            CircularProgressIndicator(
                                modifier = Modifier.size(18.dp),
                                strokeWidth = 2.dp,
                                color = MaterialTheme.colorScheme.onPrimary,
                            )
                            Text("Pairing…")
                        }
                    } else {
                        Text("Pair")
                    }
                }
            }
        },
    ) { padding ->
        Column(
            Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(scroll)
                .padding(horizontal = 20.dp, vertical = 8.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Column(
                Modifier.widthIn(max = 560.dp).fillMaxWidth(),
                verticalArrangement = Arrangement.spacedBy(20.dp),
            ) {
                Step("1 · On the machine") {
                    Surface(
                        color = MaterialTheme.colorScheme.surfaceContainer,
                        shape = RoundedCornerShape(16.dp),
                    ) {
                        Text(
                            "herder pair",
                            style = MaterialTheme.typography.titleMedium,
                            fontFamily = FontFamily.Monospace,
                            modifier = Modifier.padding(16.dp).fillMaxWidth(),
                        )
                    }
                    Text(
                        "Run this on the machine you want to add. It prints a QR code and a link that works once.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Step("2 · Scan the QR code it prints, or paste the link") {
                    FilledTonalButton(
                        onClick = ::startScan,
                        modifier = Modifier
                            .fillMaxWidth()
                            .semantics { contentDescription = "Scan QR code" },
                    ) {
                        Icon(
                            painterResource(R.drawable.ic_qr_code_scanner),
                            contentDescription = null,
                            modifier = Modifier.padding(end = 8.dp),
                        )
                        Text("Scan QR code")
                    }
                    OutlinedTextField(
                        value = link,
                        onValueChange = {
                            link = it
                            error = null
                        },
                        placeholder = { Text("herder://pair?host=…&fp=…&code=…") },
                        minLines = 3,
                        maxLines = 6,
                        textStyle = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                        shape = RoundedCornerShape(16.dp),
                        modifier = Modifier.fillMaxWidth().testTag("pairing-link"),
                    )
                    TextButton(
                        onClick = {
                            pastedLink(context)?.let { link = it }
                            error = null
                        },
                        modifier = Modifier.align(Alignment.End),
                    ) { Text("Paste") }
                }
                if (uri != null) {
                    ConfirmStep(uri, machines)
                } else if (link.isNotBlank()) {
                    Text(
                        "That is not a herder pairing link.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
                if (error != null) {
                    Text(
                        error!!,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
                Spacer(Modifier.height(8.dp))
            }
        }
    }
}

@Composable
private fun ConfirmStep(uri: ParsedPairing, machines: List<Machine>) {
    val known = machines.firstOrNull { it.fingerprint.equals(uri.fingerprint, ignoreCase = true) }
    Step(
        "3 · Check it is your machine",
        hint = "The fingerprint must match the one herder pair printed.",
    ) {
        Surface(
            color = MaterialTheme.colorScheme.surfaceContainer,
            shape = RoundedCornerShape(16.dp),
        ) {
            Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Fact("Addresses", uri.hosts.joinToString("\n"))
                Fact("Fingerprint", groupedFingerprint(uri.fingerprint), emphasize = true)
                if (known != null) {
                    Text(
                        "Already paired as ${known.name}: pairing again gives it a new key.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.primary,
                    )
                }
            }
        }
    }
}

@Composable
private fun Step(title: String, hint: String? = null, content: @Composable ColumnScope.() -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(title, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
        if (hint != null) {
            Text(
                hint,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        content()
    }
}

@Composable
private fun Fact(label: String, value: String, emphasize: Boolean = false) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(
            label,
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            value,
            style = MaterialTheme.typography.bodyLarge,
            fontFamily = FontFamily.Monospace,
            fontWeight = if (emphasize) FontWeight.SemiBold else FontWeight.Normal,
            color = if (emphasize) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface,
        )
    }
}

/** The clipboard's text, if any. */
private fun pastedLink(context: Context): String? {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    val clip = clipboard.primaryClip?.takeIf { it.itemCount > 0 } ?: return null
    return clip.getItemAt(0).coerceToText(context).toString().takeIf { it.isNotBlank() }
}

@Preview(widthDp = 411, heightDp = 891)
@Composable
private fun PairPreview() {
    HerderTheme { PairScreen(onClose = {}) }
}

@Preview(widthDp = 411, heightDp = 891)
@Composable
private fun PairConfirmPreview() {
    HerderTheme {
        PairScreen(
            machines = listOf(machine("h1", "box", ConnectionState.Connected).copy(fingerprint = SAMPLE_PAIR_FP)),
            initialLink = SAMPLE_PAIR_LINK,
            onClose = {},
        )
    }
}

@Preview(widthDp = 411, heightDp = 891)
@Composable
private fun PairScanPreview() {
    HerderTheme { PairScreen(startScanning = true, scannerPreview = true, onClose = {}) }
}
