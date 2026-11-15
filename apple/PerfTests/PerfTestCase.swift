import OSLog
import XCTest

/// What the measurements of `scripts/perf.sh` share: the app they drive, signed in to the fake
/// daemons whose links and passkey pass to `xcodebuild test` as `TEST_RUNNER_HERDER_…`, and the
/// part of each test the script reads the app's log for.
///
/// On the Mac the app is the Release copy at `HERDER_PERF_APP`, which has a bundle id of its
/// own so the installed app keeps running; on iOS, the app under test, which the scheme builds
/// for Release. `HERDER_PERF_PROFILE` names the run: the tests of a class share a profile, so
/// only the first signs in.
@MainActor
class PerfTestCase: XCTestCase {
    var app: XCUIApplication!

    private static let logger = Logger(subsystem: "sh.herder", category: "perf")

    /// Sets `app` to launch signed in to the fake daemons of the links in the environment's
    /// `links`, which it joins at launch (`Fleet.joinFakeFleet`).
    func prepare(links: [String]) throws {
        continueAfterFailure = false
        let environment = ProcessInfo.processInfo.environment
        let fleet = try links.map { try XCTUnwrap(environment[$0], "needs \($0)") }
        let passkey = try XCTUnwrap(environment["HERDER_PASSKEY"], "needs HERDER_PASSKEY")
        #if os(macOS)
        let path = try XCTUnwrap(environment["HERDER_PERF_APP"], "needs HERDER_PERF_APP")
        app = XCUIApplication(url: URL(fileURLWithPath: path))
        #else
        app = XCUIApplication()
        #endif
        let run = environment["HERDER_PERF_PROFILE"] ?? "perf-\(UUID().uuidString)"
        app.launchEnvironment["HERDER_PROFILE"] = "\(run)-\(String(describing: type(of: self)))"
        app.launchEnvironment["HERDER_FAKE_FLEET"] = fleet.joined(separator: " ")
        app.launchEnvironment["HERDER_FAKE_PASSKEY"] = passkey
        // Every turn of the main thread that cost a frame at 60 a second, not the hangs alone.
        app.launchEnvironment["HERDER_HANG_MS"] = "16"
    }

    /// Launches the app and waits for the Board to show the demo fleet.
    func launchOnDemoFleet() async throws {
        app.launch()
        let board = row("Upgrade date-fns to v4")
        #if os(iOS)
        // The fake daemons are on the Mac, across the network: iOS asks the first time.
        let allow = XCUIApplication(bundleIdentifier: "com.apple.springboard").buttons["Allow"].firstMatch
        for _ in 0..<30 where !board.exists {
            if allow.exists { allow.tap() }
            try await Task.sleep(for: .seconds(1))
        }
        #endif
        XCTAssertTrue(board.waitForExistence(timeout: 60), "the Board did not load")
    }

    /// Runs `body` as what the test measures. The app's log between the two lines this writes
    /// is the test's: perf-report.py reads its hangs (`Hangs`) and its launch (`Launch`) there.
    func measured(_ body: () -> Void) {
        let test = title
        Self.logger.notice("herder-perf begin \(test, privacy: .public)")
        body()
        Self.logger.notice("herder-perf end \(test, privacy: .public)")
    }

    /// `Board` for `-[ScrollPerfTests testBoard]`, as perf-report.py names the test.
    private var title: String {
        String(name.split(separator: " ").last?.dropLast().dropFirst("test".count) ?? "")
    }

    /// A session's row: one button labelled with its state, title, age and activity.
    func row(_ title: String) -> XCUIElement {
        app.buttons.matching(NSPredicate(format: "label CONTAINS %@", ", \(title),")).firstMatch
    }

    /// Scrolls the list on screen until `element` can be pressed: down first, then back up.
    func reveal(_ element: XCUIElement) {
        for step in 0..<12 where !element.isHittable {
            let down = step < 4
            #if os(iOS)
            if down { app.swipeUp(velocity: .slow) } else { app.swipeDown(velocity: .slow) }
            #else
            app.scrollViews.firstMatch.scroll(byDeltaX: 0, deltaY: down ? -250 : 250)
            #endif
            Thread.sleep(forTimeInterval: 0.5)
        }
    }

    func press(_ element: XCUIElement) {
        #if os(macOS)
        element.click()
        #else
        element.tap()
        #endif
    }

    /// Lets animations finish, and the screen's first layout, so the measurement sees it steady.
    func settle() {
        Thread.sleep(forTimeInterval: 2)
    }
}
