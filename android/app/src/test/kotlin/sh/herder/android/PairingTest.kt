package sh.herder.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class PairingTest {
    @Test
    fun findsTheLinkAlone() {
        assertEquals(SAMPLE_PAIR_LINK, pairingLink(SAMPLE_PAIR_LINK))
        assertEquals(SAMPLE_PAIR_LINK, pairingLink("  $SAMPLE_PAIR_LINK\n"))
    }

    @Test
    fun findsTheLinkInHerderPairOutput() {
        val output = """
            Pair a device as alice (owner): scan the code, or enter these in the app.

              address      192.168.1.5:7447
              fingerprint  $SAMPLE_PAIR_FP
              code         ABCD2345

            $SAMPLE_PAIR_LINK

            In a terminal, run `herder connect '<link>'` with it, or paste it into herder.
            """.trimIndent()
        assertEquals(SAMPLE_PAIR_LINK, pairingLink(output))
        assertEquals(SAMPLE_PAIR_LINK, pairingLink("herder connect '$SAMPLE_PAIR_LINK'"))
    }

    @Test
    fun rejectsWhatIsNotAPairingLink() {
        assertNull(pairingLink(""))
        assertNull(pairingLink("https://herder.sh"))
        assertNull(pairingLink("herder://pair?host=127.0.0.1%3A7420"))
        assertNull(pairingLink("herder://pair?fp=$SAMPLE_PAIR_FP&code=ABCD2345"))
    }

    @Test
    fun decodesAddressesAndKeepsTheCode() {
        val uri = parsePairing(SAMPLE_PAIR_LINK)
        assertEquals(listOf("192.168.1.5:7447", "10.0.0.2:7447"), uri?.hosts)
        assertEquals(SAMPLE_PAIR_FP, uri?.fingerprint)
        assertEquals("ABCD2345", uri?.code)
    }

    @Test
    fun aFingerprintReadsInGroupsOfFour() {
        assertEquals("9f2c 41ab 01de", groupedFingerprint("9F2C:41AB:01de"))
    }

    @Test
    fun acceptsAnUnencodedHost() {
        val raw = "herder://pair?host=192.168.1.5:7447&fp=$SAMPLE_PAIR_FP&code=ABCD2345"
        assertEquals(listOf("192.168.1.5:7447"), parsePairing(raw)?.hosts)
        assertEquals(raw, pairingLink(raw))
    }
}
