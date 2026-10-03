package sh.herder.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class ImagesTest {
    @Test
    fun aSmallPngGoesAsItIs() {
        val png = samplePng()
        val image = imageFromBytes(png, "image/png")
        assertEquals("image/png", image.mediaType)
        assertTrue(image.data.contentEquals(png))
    }

    @Test
    fun anUnknownTypeBecomesJpeg() {
        val image = imageFromBytes(samplePng(), "image/tiff")
        assertEquals("image/jpeg", image.mediaType)
        assertTrue(image.data.size <= MAX_IMAGE_BYTES)
    }

    @Test
    fun theDaemonTakesPngJpegGifAndWebp() {
        assertEquals(setOf("image/png", "image/jpeg", "image/gif", "image/webp"), IMAGE_MEDIA_TYPES)
    }
}
