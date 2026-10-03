package sh.herder.ffi

import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlin.test.fail
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout

/**
 * The Kotlin bindings against the fake daemon (`examples/fake_daemon.rs`), run as a sidecar:
 * connect, pair, create a session, prompt it and stream the turn.
 */
class SessionTest {
    @Test
    fun pairsAndStreamsASession() {
        val daemon = ProcessBuilder(System.getProperty("herder.fakeDaemon"))
            .redirectError(ProcessBuilder.Redirect.INHERIT)
            .start()
        try {
            val lines = daemon.inputStream.bufferedReader()
            val link = lines.readLine() ?: fail("the fake daemon printed no link")
            val repo = lines.readLine()
            val account = lines.readLine()
            runBlocking { withTimeout(60_000) { pairAndStream(link, repo, account) } }
        } finally {
            daemon.outputStream.close()
            daemon.waitFor()
        }
    }

    private suspend fun pairAndStream(link: String, repo: String, account: String) {
        assertEquals(link, pairingUriToString(parsePairingUri(link)))
        val config = Files.createTempDirectory("herder-kotlin")
        Client.open(config.toString(), "herder-kotlin-test/0").use { client ->
            val machine = client.pair(link)
            assertEquals("fake-host", machine.name)
            val host = machine.hostId
            client.synced(host)

            val created = client.send(
                host,
                CommandBody.CreateSession(
                    repo = repo,
                    projectId = null,
                    branch = null,
                    accountId = account,
                    provider = null,
                    model = null,
                    permissionMode = PermissionMode.ASK,
                    maxChildren = null,
                    failoverPin = null,
                ),
            )
            val sessionId = (created as? CommandResult.SessionCreated)?.sessionId
                ?: fail("expected a session, got $created")
            client.subscribeSession(host, sessionId).use { subscription ->
                assertEquals(
                    CommandResult.Applied,
                    client.send(host, CommandBody.SendPrompt(sessionId, "Say hello.", emptyList())),
                )
                val events = mutableListOf<EventBody>()
                while (EventBody.SessionStatusChanged(SessionStatus.IDLE, null) !in events ||
                    events.none { it is EventBody.TurnCompleted }
                ) {
                    val update = subscription.next() ?: fail("the subscription ended")
                    events += update.events.map { it.body }
                    for (item in update.streaming) {
                        (item.body as? ItemBody.AssistantMessage)?.let { println("streaming: ${it.text}") }
                    }
                }
                val answer = ItemBody.AssistantMessage("Hello, world.")
                assertTrue(events.any { it is EventBody.ItemAdded && it.item.body == answer }, "$events")
            }
        }
    }
}
