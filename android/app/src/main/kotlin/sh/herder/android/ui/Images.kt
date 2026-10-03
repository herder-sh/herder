package sh.herder.android.ui

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import sh.herder.android.R

/** Bytes of a picture, or missing after a fetch that found none. */
sealed interface ShownImage {
    data class Ready(val data: ByteArray) : ShownImage {
        override fun equals(other: Any?) = other is Ready && data.contentEquals(other.data)

        override fun hashCode() = data.contentHashCode()
    }

    data object Missing : ShownImage
}

/** The images about to go with a prompt, each removable. */
@Composable
fun PendingStrip(images: List<ByteArray>, onRemove: (Int) -> Unit) {
    if (images.isEmpty()) return
    Row(
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        modifier = Modifier.horizontalScroll(rememberScrollState()).padding(bottom = 4.dp),
    ) {
        images.forEachIndexed { index, data ->
            Box {
                Picture(
                    data = data,
                    height = 72.dp,
                    width = 96.dp,
                    label = "Pending image ${index + 1}",
                )
                IconButton(
                    onClick = { onRemove(index) },
                    modifier = Modifier.align(Alignment.TopEnd).size(28.dp),
                    colors = IconButtonDefaults.iconButtonColors(
                        containerColor = MaterialTheme.colorScheme.surface,
                        contentColor = MaterialTheme.colorScheme.onSurface,
                    ),
                ) {
                    Icon(
                        painterResource(R.drawable.ic_close),
                        contentDescription = "Remove image ${index + 1}",
                        modifier = Modifier.size(16.dp),
                    )
                }
            }
        }
    }
}

/** A user message's images: thumbnails, or a quiet placeholder when the bytes are gone. */
@Composable
fun MessageImages(images: List<ShownImage>, onOpen: (ByteArray) -> Unit) {
    if (images.isEmpty()) return
    Row(
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        modifier = Modifier.horizontalScroll(rememberScrollState()).padding(top = 8.dp),
    ) {
        images.forEachIndexed { index, image ->
            when (image) {
                is ShownImage.Ready -> Picture(
                    data = image.data,
                    height = 120.dp,
                    width = 160.dp,
                    label = "Image ${index + 1}",
                    onClick = { onOpen(image.data) },
                )
                ShownImage.Missing -> MissingImage(index + 1)
            }
        }
    }
}

/** Full-size [data], pinch to zoom; a tap outside or back dismisses. */
@Composable
fun ImageViewer(data: ByteArray, onDismiss: () -> Unit) {
    var scale by remember { mutableFloatStateOf(1f) }
    var offset by remember { mutableStateOf(Offset.Zero) }
    val bitmap = remember(data) { decode(data) }
    Dialog(
        onDismissRequest = onDismiss,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        Box(
            Modifier
                .fillMaxSize()
                .background(MaterialTheme.colorScheme.scrim.copy(alpha = 0.92f))
                .semantics { contentDescription = "Full-size image" }
                .clickable(onClick = onDismiss),
            contentAlignment = Alignment.Center,
        ) {
            if (bitmap != null) {
                Image(
                    bitmap = bitmap,
                    contentDescription = null,
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(16.dp)
                        .graphicsLayer(
                            scaleX = scale,
                            scaleY = scale,
                            translationX = offset.x,
                            translationY = offset.y,
                        )
                        .pointerInput(Unit) {
                            detectTransformGestures { _, pan, zoom, _ ->
                                scale = (scale * zoom).coerceIn(1f, 6f)
                                offset = if (scale == 1f) Offset.Zero else offset + pan
                            }
                        }
                        .clickable(enabled = false, onClick = {}),
                )
            } else {
                MissingImage(1)
            }
            IconButton(
                onClick = onDismiss,
                modifier = Modifier.align(Alignment.TopEnd).padding(8.dp),
                colors = IconButtonDefaults.iconButtonColors(contentColor = MaterialTheme.colorScheme.onPrimary),
            ) {
                Icon(painterResource(R.drawable.ic_close), contentDescription = "Close image")
            }
        }
    }
}

@Composable
private fun Picture(
    data: ByteArray,
    height: Dp,
    width: Dp,
    label: String,
    onClick: (() -> Unit)? = null,
) {
    val bitmap = remember(data) { decode(data) }
    val shape = RoundedCornerShape(12.dp)
    if (bitmap == null) {
        MissingImage(1)
        return
    }
    Image(
        bitmap = bitmap,
        contentDescription = label,
        contentScale = ContentScale.Crop,
        modifier = Modifier
            .width(width)
            .height(height)
            .clip(shape)
            .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
            .semantics { contentDescription = label },
    )
}

/** A muted box when the host no longer has the image, or it would not decode. */
@Composable
fun MissingImage(number: Int) {
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerHighest,
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier
            .width(120.dp)
            .height(80.dp)
            .semantics { contentDescription = "Missing image $number" },
    ) {
        Box(contentAlignment = Alignment.Center) {
            Icon(
                painterResource(R.drawable.ic_image),
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.45f),
                modifier = Modifier.size(28.dp),
            )
        }
    }
}

private fun decode(data: ByteArray) =
    BitmapFactory.decodeByteArray(data, 0, data.size)?.asImageBitmap()
