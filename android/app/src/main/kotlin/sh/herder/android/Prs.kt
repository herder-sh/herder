package sh.herder.android

import sh.herder.ffi.CiStatus
import sh.herder.ffi.Machine
import sh.herder.ffi.Mergeable
import sh.herder.ffi.PrState
import sh.herder.ffi.PullRequest
import sh.herder.ffi.ReviewStatus
import sh.herder.ffi.SessionStatus

// Pull requests as docs/tui-design.md §2.4 and the GTK list (linux/src/prs.rs) show them: live
// ones first, each row its number in its state's colour, its title, then its state, checks,
// review, mergeability and head branch. Checks and reviews matter only while it is open.

/** Whether the PR is still open, as a draft or not. */
fun live(pr: PullRequest): Boolean = pr.state == PrState.OPEN || pr.state == PrState.DRAFT

/** `prs`, live ones first, each group in link order. */
fun ordered(prs: List<PullRequest>): List<PullRequest> = prs.sortedBy { !live(it) }

/** A state's word, as the TUI and GTK list it. */
fun PrState.word(): String = when (this) {
    PrState.OPEN -> "open"
    PrState.DRAFT -> "draft"
    PrState.MERGED -> "merged"
    PrState.CLOSED -> "closed"
}

/** How a live check is coloured. */
enum class CheckKind { Ok, Bad, Wait, Unknown }

/** A live PR's check, as the GTK row writes it. */
data class PrCheck(val text: String, val kind: CheckKind)

/** A live PR's checks, review and mergeability; the review only once one is asked for or given. */
fun checks(pr: PullRequest): List<PrCheck> {
    if (!live(pr)) return emptyList()
    val shown = mutableListOf(
        when (pr.ci) {
            CiStatus.PASSING -> PrCheck("✓ ci", CheckKind.Ok)
            CiStatus.FAILING -> PrCheck("✗ ci", CheckKind.Bad)
            CiStatus.PENDING -> PrCheck("… ci", CheckKind.Wait)
            CiStatus.NONE -> PrCheck("– ci", CheckKind.Unknown)
        },
    )
    when (pr.review) {
        ReviewStatus.APPROVED -> shown += PrCheck("✓ approved", CheckKind.Ok)
        ReviewStatus.CHANGES_REQUESTED -> shown += PrCheck("✗ changes", CheckKind.Bad)
        ReviewStatus.REQUIRED -> shown += PrCheck("… review", CheckKind.Wait)
        ReviewStatus.NONE -> {}
    }
    shown += when (pr.mergeable) {
        Mergeable.CLEAN -> PrCheck("✓ merge", CheckKind.Ok)
        Mergeable.CONFLICTING -> PrCheck("✗ conflict", CheckKind.Bad)
        Mergeable.UNKNOWN -> PrCheck("? merge", CheckKind.Unknown)
    }
    return shown
}

/** A compact strip's CI mark, as the TUI's one-line row. */
fun ciMark(pr: PullRequest): PrCheck = when (pr.ci) {
    CiStatus.PASSING -> PrCheck("✓", CheckKind.Ok)
    CiStatus.FAILING -> PrCheck("✗", CheckKind.Bad)
    CiStatus.PENDING -> PrCheck("…", CheckKind.Wait)
    CiStatus.NONE -> PrCheck("–", CheckKind.Unknown)
}

/** Whether a conflict or requested changes holds the PR up. */
fun blocked(pr: PullRequest): Boolean =
    pr.mergeable == Mergeable.CONFLICTING || pr.review == ReviewStatus.CHANGES_REQUESTED

/** The PR number `text` names: `123`, `#123`, or a pull request URL, as the TUI reads it. */
fun parsePrNumber(text: String): ULong? {
    val trimmed = text.trim()
    val digits = trimmed.split("/pull/", limit = 2).getOrNull(1)?.let { rest ->
        rest.split('/', '#', '?').firstOrNull() ?: rest
    } ?: trimmed.removePrefix("#")
    return digits.toULongOrNull()?.takeIf { it > 0uL }
}

/**
 * Whether a session can be driven from here: not a vault's, and not archived or moved. Matches
 * the session view and the GTK list's unlink rule.
 */
fun driveable(machine: Machine?, status: SessionStatus): Boolean = when {
    machine != null && machine.hosts.isNotEmpty() -> false
    status == SessionStatus.ARCHIVED || status == SessionStatus.MOVED -> false
    else -> true
}

/** Groups that have at least one session with PRs, those sessions only. */
fun prGroups(groups: List<Group>): List<Group> = groups.mapNotNull { group ->
    val rows = group.rows.filter { it.prs.isNotEmpty() }
    if (rows.isEmpty()) null else group.copy(rows = rows)
}

/** The sidebar's count of linked PRs, as the GTK row's subtitle. */
fun prSubtitle(summaries: Map<SessionKey, Summary>): String {
    val prs = summaries.values.flatMap { it.prs }
    val open = prs.count(::live)
    return when {
        prs.isEmpty() -> "none linked"
        open == 0 -> if (prs.size == 1) "1 closed or merged" else "${prs.size} closed or merged"
        open == 1 -> "1 open"
        else -> "$open open"
    }
}
