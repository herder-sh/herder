package sh.herder.android.ui

import android.util.Log
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.mlkit.vision.MlKitAnalyzer
import androidx.camera.view.CameraController
import androidx.camera.view.LifecycleCameraController
import androidx.camera.view.PreviewView
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import sh.herder.android.R
import sh.herder.android.pairingLink

private const val TAG = "herder-qr"

/**
 * The camera, full screen, looking for the QR code `herder pair` prints; hands over the
 * pairing link in it. [preview] skips CameraX so Robolectric can screenshot the chrome.
 */
@Composable
fun PairScanner(
    onScanned: (String) -> Unit,
    onClose: () -> Unit,
    preview: Boolean = false,
) {
    var hint by remember { mutableStateOf("Point the camera at the QR code herder pair prints.") }
    var error by remember { mutableStateOf<String?>(null) }
    var found by remember { mutableStateOf(false) }

    fun accept(text: String) {
        if (found) return
        val link = pairingLink(text)
        if (link == null) {
            hint = "That QR code is not a herder pairing link."
            return
        }
        found = true
        onScanned(link)
    }

    Surface(color = Color.Black, modifier = Modifier.fillMaxSize()) {
        Box(Modifier.fillMaxSize()) {
            if (!preview) {
                CameraQrPreview(
                    onBarcode = ::accept,
                    onError = { error = it },
                    modifier = Modifier.fillMaxSize(),
                )
            }
            Box(
                Modifier
                    .size(260.dp)
                    .align(Alignment.Center)
                    .border(3.dp, Color.White.copy(alpha = 0.9f), RoundedCornerShape(24.dp)),
            )
            Column(Modifier.fillMaxSize()) {
                Row(
                    Modifier.fillMaxWidth().padding(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        "Scan the QR code",
                        style = MaterialTheme.typography.titleLarge,
                        color = Color.White,
                        modifier = Modifier.weight(1f).padding(start = 12.dp),
                    )
                    IconButton(onClick = onClose) {
                        Icon(
                            painterResource(R.drawable.ic_close),
                            contentDescription = "Close",
                            tint = Color.White,
                        )
                    }
                }
                Spacer(Modifier.weight(1f))
                Surface(
                    color = MaterialTheme.colorScheme.surface.copy(alpha = 0.92f),
                    shape = RoundedCornerShape(16.dp),
                    modifier = Modifier.padding(20.dp).fillMaxWidth(),
                ) {
                    Text(
                        error ?: hint,
                        style = MaterialTheme.typography.bodyMedium,
                        color = if (error == null) {
                            MaterialTheme.colorScheme.onSurface
                        } else {
                            MaterialTheme.colorScheme.error
                        },
                        modifier = Modifier.padding(14.dp),
                    )
                }
            }
        }
    }
}

/** Binds CameraX to ML Kit QR scanning; [onError] if this device has no usable camera. */
@Composable
private fun CameraQrPreview(
    onBarcode: (String) -> Unit,
    onError: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val scanner = remember {
        BarcodeScanning.getClient(
            BarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).build(),
        )
    }
    val controller = remember { LifecycleCameraController(context) }
    DisposableEffect(lifecycleOwner) {
        val executor = ContextCompat.getMainExecutor(context)
        controller.cameraSelector = CameraSelector.DEFAULT_BACK_CAMERA
        controller.setEnabledUseCases(CameraController.IMAGE_ANALYSIS)
        controller.setImageAnalysisAnalyzer(
            executor,
            MlKitAnalyzer(
                listOf(scanner),
                ImageAnalysis.COORDINATE_SYSTEM_ORIGINAL,
                executor,
            ) { result ->
                val barcodes = result.getValue(scanner) ?: return@MlKitAnalyzer
                barcodes.firstNotNullOfOrNull { it.rawValue }?.let(onBarcode)
            },
        )
        try {
            controller.bindToLifecycle(lifecycleOwner)
        } catch (error: RuntimeException) {
            Log.w(TAG, "camera bind failed", error)
            onError("This device has no camera herder can use. Paste the link instead.")
        }
        onDispose {
            controller.clearImageAnalysisAnalyzer()
            controller.unbind()
            scanner.close()
        }
    }
    AndroidView(
        factory = { viewContext ->
            PreviewView(viewContext).apply {
                this.controller = controller
                scaleType = PreviewView.ScaleType.FILL_CENTER
            }
        },
        modifier = modifier,
    )
}
