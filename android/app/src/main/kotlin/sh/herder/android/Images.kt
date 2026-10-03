package sh.herder.android

import android.content.ClipData
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Paint
import android.net.Uri
import java.io.ByteArrayOutputStream
import sh.herder.ffi.Image

// Prompt images: bytes in a type and size the daemon takes (`imageMediaTypes`, `maxImageBytes`),
// converted to JPEG and scaled down when they are not, as the Apple client does.
// The limits match the Kotlin bindings of those FFI helpers so Robolectric can attach
// without loading the native library.

/** Matches `herder_protocol::IMAGE_MEDIA_TYPES` / `imageMediaTypes()`. */
internal val IMAGE_MEDIA_TYPES = setOf("image/png", "image/jpeg", "image/gif", "image/webp")

/** Matches `herder_protocol::MAX_IMAGE_BYTES` / `maxImageBytes()`. */
internal const val MAX_IMAGE_BYTES = 5 * 1024 * 1024

/** Matches `herder_protocol::MAX_PROMPT_IMAGE_BYTES` / `maxPromptImageBytes()`. */
internal const val MAX_PROMPT_IMAGE_BYTES = 10 * 1024 * 1024

/** Why an image could not be attached. */
class ImageRefused(override val message: String) : Exception(message)

/** An image from a content [uri] the photo picker, camera or a paste handed over. */
fun readImage(context: Context, uri: Uri): Image =
    imageFromBytes(
        context.contentResolver.openInputStream(uri)?.use { it.readBytes() }
            ?: throw ImageRefused("That image could not be read."),
        context.contentResolver.getType(uri),
    )

/** Images on [clip], when it holds any. */
fun imagesOn(context: Context, clip: ClipData?): List<Image> {
    if (clip == null) return emptyList()
    val found = mutableListOf<Image>()
    for (index in 0 until clip.itemCount) {
        val item = clip.getItemAt(index)
        val uri = item.uri ?: continue
        runCatching { readImage(context, uri) }.getOrNull()?.let(found::add)
    }
    return found
}

/**
 * An image from its [bytes], as a picker, the camera or the clipboard gives them. Kept as-is
 * when the type is one the daemon takes and the size fits; otherwise JPEG within the limit.
 */
fun imageFromBytes(bytes: ByteArray, mediaType: String?): Image {
    val limit = MAX_IMAGE_BYTES
    val type = mediaType?.lowercase()?.substringBefore(';')
    if (type != null && type in IMAGE_MEDIA_TYPES && bytes.size <= limit) {
        return Image(type, bytes)
    }
    return Image("image/jpeg", jpegWithin(bytes, limit) ?: throw ImageRefused(tooLargeOrNotAPicture))
}

/** Whether [images] plus [next] still fit in one prompt. */
fun fitsPrompt(images: List<Image>, next: Image): Boolean =
    images.sumOf { it.data.size } + next.data.size <= MAX_PROMPT_IMAGE_BYTES

private const val tooLargeOrNotAPicture =
    "That image cannot be sent: it is not a picture, or it is too large."

/** The image as JPEG within [limit] bytes, scaling it down until it fits. */
private fun jpegWithin(bytes: ByteArray, limit: Int): ByteArray? {
    var bitmap = BitmapFactory.decodeByteArray(bytes, 0, bytes.size) ?: return null
    repeat(4) {
        val out = ByteArrayOutputStream()
        if (bitmap.compress(Bitmap.CompressFormat.JPEG, 85, out) && out.size() <= limit) {
            return out.toByteArray()
        }
        val width = bitmap.width / 2
        val height = bitmap.height / 2
        if (width < 64) return null
        bitmap = Bitmap.createScaledBitmap(bitmap, width, height, true)
    }
    return null
}

/**
 * A small PNG of [width] by [height], two bands, for previews, tests and screenshots. Not a
 * real photo: Robolectric has no gallery.
 */
fun samplePng(
    color: Int = 0xFF1B6B4A.toInt(),
    band: Int = 0xFFD8F3DC.toInt(),
    width: Int = 240,
    height: Int = 160,
): ByteArray {
    val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
    val canvas = Canvas(bitmap)
    canvas.drawColor(color)
    canvas.drawRect(
        0f,
        height / 2f,
        width.toFloat(),
        height.toFloat(),
        Paint().apply { this.color = band },
    )
    val out = ByteArrayOutputStream()
    bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
    return out.toByteArray()
}
