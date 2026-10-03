package sh.herder.android

import java.time.Duration
import java.time.Instant
import org.junit.Assert.assertEquals
import org.junit.Test
import sh.herder.ffi.CiStatus
import sh.herder.ffi.ConnectionState
import sh.herder.ffi.EventBody
import sh.herder.ffi.FleetHost
import sh.herder.ffi.Mergeable
import sh.herder.ffi.PrState
import sh.herder.ffi.SessionStatus

class ListsTest {
    private val now = Instant.parse("2026-10-03T12:00:00Z")
    private val fleet = sampleFleet(now)

    private fun lists(compact: Boolean = false) = Lists(fleet.machines, fleet.summaries, compact, now)

    private fun titles(groups: List<Group>) = groups.map { group -> group.title to group.rows.map { it.depth to it.title } }

    @Test
    fun byProjectGroupsEveryMachinesSessionsUnderTheirProject() {
        val groups = lists().groups(Scope.All, Grouping.Projects)
        assertEquals(
            listOf(
                "App" to listOf(0 to "herder/api", 1 to "write the tests", 1 to "document it", 0 to "herder/fix-login"),
                // Local: named after the repo's directory.
                "scratch" to listOf(0 to "herder/try"),
                "web" to listOf(0 to "herder/docs", 0 to "herder/login"),
            ),
            titles(groups),
        )
        assertEquals("4 sessions · box, nas", groups[0].description)
        val primary = groups[0].rows[0]
        assertEquals(2 to 1, primary.children to primary.needYou)
        assertEquals(SessionStatus.RUNNING, primary.status)
        assertEquals("box", primary.place)
        assertEquals(listOf("#12 ✓", "#9"), primary.prs.map(::badge))
        assertEquals("nas", groups[0].rows[3].place)
        assertEquals(0, groups[0].rows[1].needYou)
        assertEquals("laptop · offline", groups[2].rows[0].place)
        // Each heading shows what needs the user most.
        assertEquals(
            listOf(SessionStatus.NEEDS_YOU, SessionStatus.IDLE, SessionStatus.RUNNING),
            groups.map { it.status },
        )

        val compact = lists(compact = true).groups(Scope.Machine("h2"), Grouping.Projects)
        assertEquals(listOf("App" to listOf(0 to "fix-login")), titles(compact))
        assertEquals("1 session", compact[0].description)
    }

    @Test
    fun byMachinePutsAVaultsSessionsUnderTheirHost() {
        val groups = lists().groups(Scope.All, Grouping.Machines)
        assertEquals(
            listOf(
                "box" to listOf(
                    0 to "scratch · herder/try",
                    0 to "app · herder/api",
                    1 to "write the tests",
                    1 to "document it",
                ),
                "nas" to listOf(0 to "app · herder/fix-login"),
                "devbox" to listOf(0 to "web · herder/login"),
                "laptop" to listOf(0 to "web · herder/docs"),
            ),
            titles(groups),
        )
        assertEquals("connected · 4 sessions", groups[0].description)
        assertEquals("connection refused · 1 session", groups[1].description)
        assertEquals("online · 1 session · on vault", groups[2].description)
        assertEquals("offline · 2h 5m ago · 1 session · on vault", groups[3].description)
        assertEquals(null, groups[0].rows[0].place)

        val host = Scope.Host(vault = "v", host = "laptop")
        assertEquals(listOf("laptop" to listOf(0 to "web · herder/docs")), titles(lists().groups(host, Grouping.Machines)))
        assertEquals(listOf("web" to listOf(0 to "herder/docs")), titles(lists().groups(host, Grouping.Projects)))
    }

    @Test
    fun aSummaryFollowsBranchesAndPrs() {
        val created = Summary().applied(update("s1", created("/srv/app", "herder/a")))
        assertEquals(created, created.applied(update("s1")))
        val summary = created.applied(
            update(
                "s1",
                EventBody.BranchCheckedOut("herder/b"),
                EventBody.PrLinked(pr(7, PrState.MERGED, CiStatus.PASSING)),
                EventBody.PrLinked(pr(9, PrState.OPEN, CiStatus.PENDING)),
                EventBody.PrUpdated(pr(9, PrState.OPEN, CiStatus.FAILING)),
            ),
        )
        assertEquals("herder/b", summary.branch)
        assertEquals(listOf(pr(7, PrState.MERGED, CiStatus.PASSING), pr(9, PrState.OPEN, CiStatus.FAILING)), summary.prs)
        assertEquals(1, summary.applied(update("s1", EventBody.PrUnlinked(7u))).prs.size)
    }

    @Test
    fun anUnloadedSessionShowsItsIdAndAMovedOneWhereItWent() {
        val from = machine(
            "h1", "box", ConnectionState.Disconnected("connection refused"),
            listOf(head("s1", null).copy(status = SessionStatus.MOVED)),
        )
        val to = machine("h2", "nas", ConnectionState.Connected, listOf(head("s1", null)))
        val groups = Lists(listOf(from, to), emptyMap(), compact = false, now).groups(Scope.All, Grouping.Machines)
        assertEquals("connection refused · 1 session", groups[0].description)
        assertEquals("s1", groups[0].rows[0].title)
        assertEquals("nas", groups[0].rows[0].movedTo)
        assertEquals(null, groups[1].rows[0].movedTo)
        assertEquals(setOf(SessionKey("h1", "s1"), SessionKey("h2", "s1")), keys(listOf(from, to)))
    }

    @Test
    fun anOfflineHostSaysWhenItWasLastSeen() {
        fun host(lastSeen: String) = FleetHost(hostId = "old", hostName = "old", online = false, lastSeen = lastSeen)
        assertEquals("offline · 1d 3h ago", hostState(host(now.minus(Duration.ofHours(27)).toString()), now))
        assertEquals("offline · 4m ago", hostState(host("2026-10-03T14:56:00+03:00"), now))
        assertEquals("offline", hostState(host("yesterday"), now))
    }

    @Test
    fun aLivePrBadgeShowsItsChecksAndWhatBlocksIt() {
        assertEquals("#12 ✓", badge(pr(12, PrState.OPEN, CiStatus.PASSING)))
        assertEquals("#3 ✗!", badge(pr(3, PrState.DRAFT, CiStatus.FAILING).copy(mergeable = Mergeable.CONFLICTING)))
        assertEquals("#9", badge(pr(9, PrState.MERGED, CiStatus.FAILING)))
    }

    @Test
    fun theMachinesListOffersAllThenEachMachineAndItsHosts() {
        assertEquals(
            listOf(
                Scope.All,
                Scope.Machine("h1"),
                Scope.Machine("h2"),
                Scope.Machine("v"),
                Scope.Host("v", "devbox"),
                Scope.Host("v", "laptop"),
            ),
            scopes(fleet.machines),
        )
        assertEquals(emptyList<Scope>(), scopes(emptyList()))
        assertEquals("laptop", scopeTitle(fleet.machines, Scope.Host("v", "laptop")))
    }
}
