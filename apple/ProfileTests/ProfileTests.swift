import XCTest

/// Holds the Mac app on a scene for `profile-mac.sh` to record: launches the copy of the app at
/// `HERDER_PROFILE_APP` on a fresh profile, pairs it with `fake_daemon --busy` through
/// `HERDER_PAIR_LINK`, opens the scene `HERDER_PROFILE_SCENE` names (`session`, the busy
/// session's streaming transcript, or `board`), prints `herder-profile-ready <the app's pid>`
/// and waits there `HERDER_PROFILE_SECONDS`. Pass each to `xcodebuild test` as
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

        let add = app.buttons["Add Machine"].firstMatch
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

        if scene == "session" {
            let row = app.buttons.matching(
                NSPredicate(format: "label CONTAINS %@", ", A long transcript, still streaming,")).firstMatch
            XCTAssertTrue(row.waitForExistence(timeout: 15), app.debugDescription)
            row.click()
            XCTAssertTrue(app.descendants(matching: .any)["transcript"].waitForExistence(timeout: 15))
        }
        // Past the scene's first layout, so the recording sees it steady.
        Thread.sleep(forTimeInterval: 3)
        let pid = try XCTUnwrap(NSRunningApplication.runningApplications(
            withBundleIdentifier: "sh.herder.Herder.Profile").first?.processIdentifier, "the app is not running")
        print("herder-profile-ready \(pid)")
        fflush(stdout)
        Thread.sleep(forTimeInterval: seconds)
    }
}
