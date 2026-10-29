import XCTest

/// Holds the Mac app on a scene for `profile-mac.sh` to record: launches the copy of the app at
/// `HERDER_PROFILE_APP` on a fresh profile, pairs it with `fake_daemon --busy` through
/// `HERDER_PAIR_LINK`, opens the scene `HERDER_PROFILE_SCENE` names (`session`, the busy
/// session's streaming transcript; `board`; or `switch`, which goes back and forth between the
/// busy session and a finished one every two seconds), prints `herder-profile-ready <the app's
/// pid>` and holds the scene `HERDER_PROFILE_SECONDS`. Pass each to `xcodebuild test` as
/// `TEST_RUNNER_<NAME>`. The runner is sandboxed, so it says it is ready on its output, which
/// xcodebuild passes on, not in a file.
@MainActor
final class ProfileTests: XCTestCase {
    func testHoldScene() throws {
        let environment = ProcessInfo.processInfo.environment
        let path = try XCTUnwrap(environment["HERDER_PROFILE_APP"], "needs HERDER_PROFILE_APP")
        let link = try XCTUnwrap(environment["HERDER_PAIR_LINK"], "needs HERDER_PAIR_LINK")
        let scene = environment["HERDER_PROFILE_SCENE"] ?? "session"
        let seconds = Double(environment["HERDER_PROFILE_SECONDS"] ?? "") ?? 20

        // A copy with its own bundle id, so launching it leaves the installed app running.
        let app = XCUIApplication(url: URL(fileURLWithPath: path))
        app.launchEnvironment["HERDER_PROFILE"] = "profile-\(UUID().uuidString)"
        app.launch()

        let add = app.buttons["add-to-fleet"].firstMatch
        XCTAssertTrue(add.waitForExistence(timeout: 15), app.debugDescription)
        add.click()
        let field = app.descendants(matching: .any)["pairing-link"].firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5), app.debugDescription)
        field.click()
        field.typeText(link)
        app.buttons["Pair"].firstMatch.click()
        let done = app.buttons["Done"].firstMatch
        XCTAssertTrue(done.waitForExistence(timeout: 30), app.debugDescription)
        done.click()

        let busy = row(in: app, titled: "A long transcript, still streaming")
        let finished = row(in: app, titled: "Tidy the README badges")
        if scene != "board" {
            XCTAssertTrue(busy.waitForExistence(timeout: 15), app.debugDescription)
            busy.click()
            XCTAssertTrue(app.descendants(matching: .any)["transcript"].waitForExistence(timeout: 15))
        }
        // Past the scene's first layout, so the recording sees it steady.
        Thread.sleep(forTimeInterval: 3)
        let pid = try XCTUnwrap(NSRunningApplication.runningApplications(
            withBundleIdentifier: "sh.herder.Herder.Profile").first?.processIdentifier, "the app is not running")
        print("herder-profile-ready \(pid)")
        fflush(stdout)
        guard scene == "switch" else {
            Thread.sleep(forTimeInterval: seconds)
            return
        }
        let end = Date.now.addingTimeInterval(seconds)
        var rows = [finished, busy]
        while Date.now < end {
            rows[0].click()
            rows.reverse()
            Thread.sleep(forTimeInterval: 2)
        }
    }

    private func row(in app: XCUIApplication, titled title: String) -> XCUIElement {
        app.buttons.matching(NSPredicate(format: "label CONTAINS %@", ", \(title),")).firstMatch
    }
}
