package sh.herder.android

import java.net.URLDecoder
import java.nio.charset.StandardCharsets

/**
 * The `herder://pair` link in text from a QR code or the clipboard: the link alone, or a copy
 * of `herder pair`'s output with the link somewhere in it. Null when there is none.
 */
fun pairingLink(text: String): String? =
    text.split(Regex("\\s+"))
        .asSequence()
        .map { it.trim('\'', '"', '<', '>', '`', '(', ')') }
        .firstOrNull { parsePairing(it) != null }

/**
 * What a `herder://pair` link names, matching `herder-client-core::PairingUri` so the
 * fingerprint can be shown before `Client.pair` is called.
 */
data class ParsedPairing(
    val hosts: List<String>,
    val fingerprint: String,
    val code: String,
)

/** Parses a `herder://pair?host=…&fp=…&code=…` link; null when it is not one. */
fun parsePairing(link: String): ParsedPairing? {
    if (!link.startsWith("herder://pair?")) return null
    val hosts = mutableListOf<String>()
    var fingerprint: String? = null
    var code: String? = null
    for (part in link.removePrefix("herder://pair?").split('&')) {
        val eq = part.indexOf('=')
        if (eq <= 0) continue
        val value = decodeQuery(part.substring(eq + 1))
        when (part.substring(0, eq)) {
            "host" -> if (value.isNotEmpty()) hosts.add(value)
            "fp" -> fingerprint = value
            "code" -> code = value
        }
    }
    val fp = fingerprint?.takeIf { it.isNotEmpty() } ?: return null
    val pairingCode = code?.takeIf { it.isNotEmpty() } ?: return null
    if (hosts.isEmpty()) return null
    return ParsedPairing(hosts, fp, pairingCode)
}

/** A fingerprint in groups of four hex digits, as people compare them. */
fun groupedFingerprint(fingerprint: String): String =
    fingerprint
        .filter { it.isDigit() || it in 'a'..'f' || it in 'A'..'F' }
        .lowercase()
        .chunked(4)
        .joinToString(" ")

private fun decodeQuery(value: String): String =
    URLDecoder.decode(value.replace("+", "%2B"), StandardCharsets.UTF_8)

/** A pairing link used by previews and tests, as `herder pair` prints it. */
internal val SAMPLE_PAIR_FP = "ab".repeat(32)

internal val SAMPLE_PAIR_LINK =
    "herder://pair?host=192.168.1.5%3A7447&host=10.0.0.2%3A7447&fp=$SAMPLE_PAIR_FP&code=ABCD2345"
