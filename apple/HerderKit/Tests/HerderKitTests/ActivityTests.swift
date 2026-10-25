import Foundation
import Herder
@testable import HerderKit
import Testing

struct ActivityTests {
    private func install(_ provider: Provider, on hostId: HostId) -> JobKey {
        JobKey(hostId: hostId, kind: .install(provider))
    }

    @Test func installsRunOneAtATimePerMachineAndMachinesInParallel() {
        let claudeBox = install("claude", on: "box")
        let codexBox = install("codex", on: "box")
        let claudeLaptop = install("claude", on: "laptop")
        let login = JobKey(hostId: "box", kind: .add("claude-2"))
        // Update All on two machines: the first install on each starts, the second on box waits.
        #expect(Activity.startable([(claudeBox, .queued), (codexBox, .queued), (claudeLaptop, .queued)])
            == [claudeBox, claudeLaptop])
        // While box's first runs, its next stays queued; a login there starts at once.
        #expect(Activity.startable([(claudeBox, .running), (codexBox, .queued), (login, .queued)]) == [login])
        // Once it ended, the next one's turn comes.
        #expect(Activity.startable([(claudeBox, .ended), (codexBox, .queued), (claudeLaptop, .running)]) == [codexBox])
        // A login waiting on the user never holds the installs back.
        #expect(Activity.startable([(login, .running), (codexBox, .queued)]) == [codexBox])
    }

    @MainActor
    @Test func aJobQueuesOnceAndRunsAgainOnlyOnceEnded() {
        var activity = Activity()
        let key = install("claude", on: "box")
        let first = TerminalConnection(hostId: "box", terminalId: nil, install: "claude")
        let queued = activity.enqueue(key, first)
        #expect(queued)
        #expect(activity.phase(key) == .queued && activity.startable == [key])
        // The same job again while it is queued or runs is the one there.
        let again = { activity.enqueue(key, TerminalConnection(hostId: "box", terminalId: nil, install: "claude")) }
        #expect(!again())
        activity.start(key, before: "2.1.303")
        #expect(activity.phase(key) == .running && activity[key]?.before == "2.1.303")
        #expect(!again())
        #expect(activity[key]?.connection === first)
        activity.end(key)
        #expect(activity.phase(key) == .ended)
        // Retried: a new job in its place, at the end of the list.
        let other = install("codex", on: "box")
        activity.enqueue(other, TerminalConnection(hostId: "box", terminalId: nil, install: "codex"))
        #expect(again())
        #expect(activity.keys == [other, key] && activity.phase(key) == .queued)
        activity.clear(key)
        #expect(activity[key] == nil && activity.keys == [other])
    }

    @MainActor
    @Test func loginsStillRunningHoldTheirAccountIds() {
        var activity = Activity()
        let account = NewAccount(accountId: "claude-2", provider: "claude", label: nil, configDir: nil)
        let add = JobKey(hostId: "box", kind: .add("claude-2"))
        activity.enqueue(add, TerminalConnection(hostId: "box", terminalId: nil, account: account))
        activity.enqueue(JobKey(hostId: "laptop", kind: .add("codex")),
                         TerminalConnection(hostId: "laptop", terminalId: nil, account: account))
        #expect(activity.pendingIds(on: "box") == ["claude-2"])
        activity.start(add, before: nil)
        activity.end(add)
        #expect(activity.pendingIds(on: "box").isEmpty)
    }

    @Test func outcomesMapToTheirGroups() {
        #expect(JobProgress.queued.group == .queued)
        #expect(JobProgress.starting.group == .running && JobProgress.installing.group == .running)
        // A login waits on the user: amber, Needs you.
        #expect(JobProgress.running.group == .needsYou)
        let device = AwsDeviceCode(verificationUrl: "https://device.sso.aws/?user_code=ABCD-EFGH", userCode: "ABCD-EFGH")
        #expect(JobProgress.approve(device).group == .needsYou)
        for done in [JobProgress.added, .signedIn, .ended, .installed("1"), .updated(from: "1", to: "2"),
                     .upToDate("2"), .failed("no")] {
            #expect(done.group == .done && done.finished)
        }
        #expect(JobProgress.failed("no").failed && !JobProgress.added.failed)
    }

    @Test func aLoginRunAgainSaysHowItEnded() {
        #expect(JobProgress(relogin: .connecting) == .starting)
        #expect(JobProgress(relogin: .attached) == .running)
        #expect(JobProgress(relogin: .exited(0)) == .signedIn)
        #expect(JobProgress(relogin: .exited(1)) == .failed("The login exited with 1."))
        // Cancelled: its terminal closed without a code.
        #expect(JobProgress(relogin: .exited(nil)) == .ended)
        #expect(JobProgress(relogin: .failed("offline")) == .failed("offline"))
    }

    @Test func anAwsSignInSaysWhatItWaitsFor() {
        let device = AwsDeviceCode(verificationUrl: "https://device.sso.aws/", userCode: "ABCD-EFGH")
        #expect(JobProgress(AwsSignInStatus.starting) == .starting)
        #expect(JobProgress(AwsSignInStatus.waiting(device)) == .approve(device))
        #expect(JobProgress(AwsSignInStatus.signedIn) == .signedIn)
        #expect(JobProgress(AwsSignInStatus.failed(2)) == .failed("aws sso login exited with 2."))
        #expect(JobProgress(AwsSignInStatus.notInstalled) == .failed("The AWS CLI is not installed on the machine."))
        #expect(JobProgress(AwsSignInStatus.refused("Terminals are owner-only.")) == .failed("Terminals are owner-only."))
    }

    @Test func rowsNameTheJobAndItsOutcome() {
        let update = install("codex", on: "box")
        #expect(update.title(machine: "work-mbp", provider: "codex", progress: .installing, updating: true)
            == "Update Codex on work-mbp")
        #expect(update.title(machine: "work-mbp", provider: "codex", progress: .queued, updating: false)
            == "Install Codex on work-mbp")
        #expect(update.title(machine: "work-mbp", provider: "claude", progress: .updated(from: "2.1.303", to: "2.1.310"),
                             updating: true) == "Updated Claude on work-mbp")
        #expect(update.title(machine: "trash-can-01", provider: "opencode", progress: .failed("exit 243"), updating: true)
            == "OpenCode on trash-can-01 failed")
        let login = JobKey(hostId: "box", kind: .add("claude-ops"))
        #expect(login.title(machine: "box", provider: "claude", progress: .running, updating: false)
            == "Sign in to Claude · claude-ops on box")
        let aws = JobKey(hostId: "box", kind: .awsSignIn(profile: "prod"))
        #expect(aws.title(machine: "box", provider: "", progress: .signedIn, updating: false) == "Signed in to AWS prod on box")
        #expect(JobProgress.updated(from: "2.1.303", to: "2.1.310").detail(machine: "box", before: "2.1.303")
            == "2.1.303 → 2.1.310")
        #expect(JobProgress.installing.detail(machine: "box", before: "0.48.0") == "From 0.48.0 · running the vendor’s updater")
        #expect(JobProgress.queued.detail(machine: "box", before: nil) == "Queued · one installer at a time on box")
        #expect(JobProgress.failed("The installer exited with 243.").detail(machine: "box", before: nil)
            == "The installer exited with 243.")
    }

    @Test func doneListsNewestFirstAndOtherGroupsInTheirOrder() {
        let text = JobText(machine: "box", title: "", detail: "")
        let old = ActivityItem(key: install("claude", on: "box"), progress: .installed("1"), text: text,
                               ended: Date(timeIntervalSince1970: 10))
        let new = ActivityItem(key: install("codex", on: "box"), progress: .failed("no"), text: text,
                               ended: Date(timeIntervalSince1970: 20))
        let running = ActivityItem(key: install("grok", on: "box"), progress: .installing, text: text)
        let waiting = ActivityItem(key: JobKey(hostId: "box", kind: .add("a")), progress: .running, text: text)
        let groups = ActivityList.grouped([old, running, new, waiting])
        #expect(groups.map(\.group) == [.needsYou, .running, .done])
        #expect(groups.last?.members.map(\.key) == [new.key, old.key])
    }

    @Test func theLogDropsEscapesAndRedrawnLines() {
        let bytes = Array("\u{1B}[33mWarning:\u{1B}[0m two\r\n\u{1B}]0;title\u{07}10%\r55%\r100%\nDone.\n".utf8)
        #expect(TerminalLog.plain(bytes) == "Warning: two\n100%\nDone.\n")
    }

    @Test func claudesSecondInstallIsExplainedWithItsFix() {
        let log = """
            Current version: 2.1.303
            Checking for updates to latest version...

            Warning: Multiple installations found
            - npm-global at /opt/homebrew/bin/claude
            - native at ~/.local/bin/claude (currently running)
            Updating to 2.1.310...
            npm warn deprecated inflight@1.0.6: not supported
            WARN: PATH is missing ~/.local/bin
            Successfully updated from 2.1.303 to version 2.1.310
            """
        let warnings = VendorWarning.read(log, provider: "claude")
        #expect(warnings.map(\.title) == ["2 Claude installs on this machine", "PATH is missing ~/.local/bin"])
        #expect(warnings.first?.fix == "npm uninstall -g @anthropic-ai/claude-code")
        #expect(warnings.first?.detail.contains("/opt/homebrew/bin/claude") == true)
        #expect(warnings.first?.detail.contains("~/.local/bin/claude.") == true)
        // Another provider's installer is not read for Claude's copies.
        #expect(VendorWarning.read(log, provider: "codex").map(\.title) == ["PATH is missing ~/.local/bin"])
        #expect(VendorWarning.read("Done.\n", provider: "claude").isEmpty)
    }

    @MainActor
    @Test(.enabled(if: FakeDaemon.path != nil, "needs HERDER_FAKE_DAEMON"))
    func updateAllOnTwoMachinesQueuesThenRunsThenEnds() async throws {
        let one = try FakeDaemon(name: "one")
        let two = try FakeDaemon(name: "two")
        guard case .opened(let fleet) = Profile.open(at: temporaryProfile(), client: "test") else {
            Issue.record("cannot open profile")
            return
        }
        let first = try await fleet.pair(one)
        let second = try await fleet.pair(two)
        try await fleet.client.synced(hostId: first.hostId)
        try await fleet.client.synced(hostId: second.hostId)
        let keys = [(first.hostId, "claude"), (first.hostId, "codex"), (second.hostId, "claude"), (second.hostId, "codex")]
            .map { fleet.install($0.1, on: $0.0) }
        // One installer per machine runs; each machine's second waits for it.
        #expect(keys.map { fleet.activity.phase($0) } == [.running, .queued, .running, .queued])
        #expect(keys.map { fleet.progress($0).group } == [.running, .queued, .running, .queued])
        // The fake daemons install nothing and say so, so each ends failed and the next starts.
        #expect(await eventually(within: 30) { keys.allSatisfy { fleet.activity.phase($0) == .ended } })
        #expect(keys.allSatisfy { fleet.progress($0).failed })
        // Retry runs one again; Clear finished then leaves only it.
        fleet.retry(keys[0])
        #expect(fleet.activity.phase(keys[0]) == .running)
        fleet.clearFinished()
        #expect(fleet.activity.keys == [keys[0]])
    }
}
