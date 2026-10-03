package sh.herder.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.herder.ffi.CiStatus
import sh.herder.ffi.ConnectionState
import sh.herder.ffi.Mergeable
import sh.herder.ffi.PrState
import sh.herder.ffi.ReviewStatus
import sh.herder.ffi.SessionStatus

class PrsTest {
    @Test
    fun numbersAndLinksNameAPullRequest() {
        assertEquals(42uL, parsePrNumber("42"))
        assertEquals(42uL, parsePrNumber(" #42 "))
        assertEquals(42uL, parsePrNumber("https://github.com/acme/app/pull/42/files#diff"))
        assertNull(parsePrNumber("github.com/acme/app/issues/42"))
        assertNull(parsePrNumber("0"))
        assertNull(parsePrNumber(""))
    }

    @Test
    fun checksReadAsTheTuisAndOnlyForLivePrs() {
        val open = pr(12, PrState.OPEN, CiStatus.PASSING).copy(
            review = ReviewStatus.CHANGES_REQUESTED,
            mergeable = Mergeable.CONFLICTING,
        )
        assertEquals(
            listOf(
                PrCheck("✓ ci", CheckKind.Ok),
                PrCheck("✗ changes", CheckKind.Bad),
                PrCheck("✗ conflict", CheckKind.Bad),
            ),
            checks(open),
        )
        assertEquals(
            listOf(PrCheck("… ci", CheckKind.Wait), PrCheck("✓ merge", CheckKind.Ok)),
            checks(pr(9, PrState.DRAFT, CiStatus.PENDING)),
        )
        assertTrue(checks(pr(7, PrState.MERGED, CiStatus.PASSING)).isEmpty())
        assertEquals(
            listOf(9uL, 12uL, 7uL, 3uL),
            ordered(
                listOf(
                    pr(7, PrState.MERGED, CiStatus.NONE),
                    pr(9, PrState.DRAFT, CiStatus.NONE),
                    pr(3, PrState.CLOSED, CiStatus.NONE),
                    pr(12, PrState.OPEN, CiStatus.NONE),
                ),
            ).map { it.number },
        )
    }

    @Test
    fun theSidebarCountsOpenPrs() {
        val now = java.time.Instant.parse("2026-10-03T12:00:00Z")
        assertEquals("2 open", prSubtitle(sampleFleet(now).summaries))
        assertEquals("none linked", prSubtitle(emptyMap()))
        val closed = mapOf(
            SessionKey("h1", "s1") to Summary(loaded = true, prs = listOf(pr(1, PrState.MERGED, CiStatus.PASSING))),
        )
        assertEquals("1 closed or merged", prSubtitle(closed))
    }

    @Test
    fun aVaultOrArchivedSessionCannotBeDriven() {
        val box = machine("h1", "box", ConnectionState.Connected)
        val vault = box.copy(hosts = listOf(sh.herder.ffi.FleetHost("devbox", "devbox", true, "2026-10-03T12:00:00Z")))
        assertTrue(driveable(box, SessionStatus.IDLE))
        assertFalse(driveable(vault, SessionStatus.IDLE))
        assertFalse(driveable(box, SessionStatus.ARCHIVED))
        assertFalse(driveable(box, SessionStatus.MOVED))
    }

    @Test
    fun prGroupsKeepOnlySessionsWithPrs() {
        val now = java.time.Instant.parse("2026-10-03T12:00:00Z")
        val groups = prGroups(Lists(sampleFleet(now).machines, sampleFleet(now).summaries, compact = false, now).groups(Scope.All, Grouping.Projects))
        assertEquals(listOf("App", "web"), groups.map { it.title })
        assertEquals(listOf("herder/api", "write the tests"), groups[0].rows.map { it.title })
        assertEquals(listOf("herder/docs"), groups[1].rows.map { it.title })
    }
}
