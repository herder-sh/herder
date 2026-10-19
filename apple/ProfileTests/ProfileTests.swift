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

        let busy = row(in: app, titled: "A long transcript, still streaming")
        let finished = row(in: app, titled: "Rate-limit the public API per key")
        if scene.hasPrefix("copy") { app.buttons["api"].firstMatch.click(); Thread.sleep(forTimeInterval: 1) }
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
        if scene == "copy" || scene == "copybusy" {
            if scene == "copy", finished.waitForExistence(timeout: 3) {

                finished.click()
                Thread.sleep(forTimeInterval: 3)
            }
            let composer = app.descendants(matching: .any)["composer"].firstMatch
            NSPasteboard.general.clearContents()
            composer.click()
            composer.typeText("control text")
            app.typeKey("a", modifierFlags: .command)
            app.typeKey("c", modifierFlags: .command)
            Thread.sleep(forTimeInterval: 0.5)
            print("herder-copy control: \((NSPasteboard.general.string(forType: .string) ?? "<nil>").debugDescription)")
            NSPasteboard.general.clearContents()
            app.typeKey(.rightArrow, modifierFlags: [])
            composer.coordinate(withNormalizedOffset: CGVector(dx: 0.0, dy: 0.0)).withOffset(CGVector(dx: 2, dy: 8))
                .press(forDuration: 0.2, thenDragTo: composer.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: 60, dy: 8)), withVelocity: 300, thenHoldForDuration: 0.3)
            app.typeKey("c", modifierFlags: .command)
            Thread.sleep(forTimeInterval: 0.5)
            print("herder-copy composer drag: \((NSPasteboard.general.string(forType: .string) ?? "<nil>").debugDescription)")
            app.typeKey("a", modifierFlags: .command)
            app.typeKey(.delete, modifierFlags: [])
            let transcript = app.descendants(matching: .any)["transcript"].firstMatch
            let texts = transcript.staticTexts.allElementsBoundByIndex.filter { $0.isHittable && $0.frame.width > 200 && $0.frame.height > 30 }
            print("herder-copy texts \(texts.count)")
            for delay in [0.0, 0.3, 0.5, 1.0, 0.0, 0.3, 0.5, 1.0] {
                guard let text = texts.first else { break }
                print("herder-copy target \(text.frame) \(String(describing: text.value).prefix(50))")
                NSPasteboard.general.clearContents()
                let start = text.coordinate(withNormalizedOffset: CGVector(dx: 0.02, dy: 0.1))
                let end = text.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.9))
                start.press(forDuration: 0.2, thenDragTo: end, withVelocity: 300, thenHoldForDuration: 0.3)
                if delay > 0 { Thread.sleep(forTimeInterval: delay) }
                try? app.windows.firstMatch.screenshot().pngRepresentation.write(to: URL(fileURLWithPath: NSTemporaryDirectory() + "copy-\(delay).png"))
                print("herder-copy shot " + NSTemporaryDirectory())
                app.typeKey("c", modifierFlags: .command)
                Thread.sleep(forTimeInterval: 0.5)
                let got = NSPasteboard.general.string(forType: .string) ?? "<nil>"
                print("herder-copy delay \(delay): \(got.prefix(60).debugDescription) len \(got.count)")
                app.windows.firstMatch.coordinate(withNormalizedOffset: CGVector(dx: 0.7, dy: 0.3)).click()
                Thread.sleep(forTimeInterval: 0.5)
            }
            return
        }
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
