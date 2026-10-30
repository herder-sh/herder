import XCTest

/// Measures how smoothly each long surface of the app scrolls, for `scripts/perf.sh` to read
/// out of the result bundle and hold to the budget: on the demo fleet under load
/// (`fake_daemon --busy`, whose link and passkey pass to `xcodebuild test` as
/// `TEST_RUNNER_HERDER_PAIR_LINK` and `TEST_RUNNER_HERDER_PASSKEY`), each test opens one screen
/// and scrolls it to its end and back with `XCTOSSignpostMetric.scrollingAndDecelerationMetric`,
/// whose hitch time ratio is the number that counts.
///
/// On the Mac it drives the Release copy of the app at `HERDER_PERF_APP`, which has a bundle
/// id of its own so the installed app keeps running; on the iPhone, the app under test, which
/// the scheme builds for Release. `HERDER_PERF_PROFILE` names the profile every test of a run
/// shares, so only the first signs in.
@MainActor
final class ScrollPerfTests: XCTestCase {
    private var app: XCUIApplication!

    override func setUp() async throws {
        continueAfterFailure = false
        let environment = ProcessInfo.processInfo.environment
        let link = try XCTUnwrap(environment["HERDER_PAIR_LINK"], "needs HERDER_PAIR_LINK")
        let passkey = try XCTUnwrap(environment["HERDER_PASSKEY"], "needs HERDER_PASSKEY")
        #if os(macOS)
        let path = try XCTUnwrap(environment["HERDER_PERF_APP"], "needs HERDER_PERF_APP")
        app = XCUIApplication(url: URL(fileURLWithPath: path))
        #else
        app = XCUIApplication()
        #endif
        app.launchEnvironment["HERDER_PROFILE"] = environment["HERDER_PERF_PROFILE"] ?? "perf-\(UUID().uuidString)"
        // The app joins the fake daemons at launch (`Fleet.joinFakeFleet`).
        app.launchEnvironment["HERDER_FAKE_FLEET"] = link
        app.launchEnvironment["HERDER_FAKE_PASSKEY"] = passkey
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

    func testBoard() {
        section("Board")
        measureScrolling()
    }

    /// The busy session: about 2,000 items, its last turn streaming a round a second.
    func testTranscript() {
        let session = row("A long transcript, still streaming")
        reveal(session)
        press(session)
        XCTAssertTrue(app.descendants(matching: .any)["transcript"].waitForExistence(timeout: 15))
        // It opens at its end, so it scrolls up first.
        measureScrolling(up: true)
    }

    /// A finished session with child sessions, artifact cards and a PR.
    func testTranscriptWithChildren() {
        let session = row("Roll out 30-day log retention")
        reveal(session)
        press(session)
        XCTAssertTrue(app.descendants(matching: .any)["transcript"].waitForExistence(timeout: 15))
        measureScrolling(up: true)
    }

    /// The conversations' list.
    func testChats() {
        section("Chats")
        measureScrolling()
    }

    func testBots() {
        #if os(macOS)
        section("Bots")
        #else
        section("Chats")
        let bots = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Bots'")).firstMatch
        XCTAssertTrue(bots.waitForExistence(timeout: 10))
        press(bots)
        #endif
        XCTAssertTrue(row("Why invoice exports time out").waitForExistence(timeout: 15))
        measureScrolling()
    }

    func testFleet() {
        section("Fleet")
        measureScrolling()
    }

    func testPeople() {
        #if os(macOS)
        section("People")
        #else
        link("people-link")
        #endif
        measureScrolling()
    }

    func testProviders() {
        #if os(macOS)
        section("Providers")
        #else
        link("providers-link")
        #endif
        measureScrolling()
    }

    /// A machine's settings: its accounts, CLIs, limits, MCP servers and backup.
    func testMachineSettings() {
        #if os(macOS)
        let footer = app.descendants(matching: .any)["sidebar-machines"].firstMatch
        XCTAssertTrue(footer.waitForExistence(timeout: 10))
        press(footer)
        let devbox = app.descendants(matching: .any)["sidebar-machine-devbox"].firstMatch
        XCTAssertTrue(devbox.waitForExistence(timeout: 10))
        press(devbox)
        #else
        section("Fleet")
        let devbox = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'devbox'")).firstMatch
        XCTAssertTrue(devbox.waitForExistence(timeout: 10))
        reveal(devbox)
        press(devbox)
        #endif
        XCTAssertTrue(app.buttons["section-index-MCP servers"].waitForExistence(timeout: 15))
        measureScrolling()
    }

    /// Scrolls the screen to its end and back, or to its start and back with `up`, as each
    /// iteration of the measurement: a fling at a time, as a person reads through a list.
    private func measureScrolling(up: Bool = false) {
        settle()
        #if os(macOS)
        // The widest list on screen: the sidebar and the list pane are narrower than the screen's.
        let list = app.scrollViews.allElementsBoundByIndex.filter(\.isHittable).max { $0.frame.width < $1.frame.width }
        let target: XCUIElement = list ?? app.windows.firstMatch
        #else
        let target: XCUIElement = app
        #endif
        let options = XCTMeasureOptions()
        options.iterationCount = 5
        measure(metrics: [XCTOSSignpostMetric.scrollingAndDecelerationMetric], options: options) {
            for _ in 0..<Self.flings { fling(target, up: up) }
            for _ in 0..<Self.flings { fling(target, up: !up) }
        }
    }

    private static let flings = 4

    /// One fast fling of `list` towards its end, or its start with `up`.
    private func fling(_ list: XCUIElement, up: Bool) {
        #if os(macOS)
        list.scroll(byDeltaX: 0, deltaY: up ? 1500 : -1500)
        #else
        if up { list.swipeDown(velocity: .fast) } else { list.swipeUp(velocity: .fast) }
        #endif
    }

    /// Switches to the section named `name`: a tab on the iPhone, a sidebar row on the Mac.
    private func section(_ name: String) {
        #if os(iOS)
        let tab = app.tabBars.buttons[name].firstMatch
        XCTAssertTrue(tab.waitForExistence(timeout: 10), "no \(name) tab")
        tab.tap()
        #else
        let row = app.buttons.matching(NSPredicate(
            format: "label == %@ OR label BEGINSWITH %@", name, "\(name), ")).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10), "no \(name) in the sidebar")
        row.click()
        #endif
        settle()
    }

    #if os(iOS)
    /// Opens one of the Fleet tab's links, such as Providers.
    private func link(_ id: String) {
        section("Fleet")
        let link = app.descendants(matching: .any)[id].firstMatch
        XCTAssertTrue(link.waitForExistence(timeout: 10), "no \(id)")
        reveal(link)
        press(link)
        settle()
    }
    #endif

    /// A session's row: one button labelled with its state, title, age and activity.
    private func row(_ title: String) -> XCUIElement {
        app.buttons.matching(NSPredicate(format: "label CONTAINS %@", ", \(title),")).firstMatch
    }

    /// Scrolls the list on screen until `element` can be pressed: down first, then back up.
    private func reveal(_ element: XCUIElement) {
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

    private func press(_ element: XCUIElement) {
        #if os(macOS)
        element.click()
        #else
        element.tap()
        #endif
    }

    /// Lets animations finish, and the screen's first layout, so the measurement sees it steady.
    private func settle() {
        Thread.sleep(forTimeInterval: 2)
    }
}
