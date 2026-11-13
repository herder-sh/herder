import XCTest

/// Measures how the app opens, resumes and shows a long session on a fleet of a real one's
/// size: the demo fleet under load (`fake_daemon --busy`, in `HERDER_PAIR_LINK`) and a machine
/// with a used journal (`fake_daemon --load big …`, in `HERDER_LOAD_LINK`). `scripts/perf.sh`
/// reads what each test measured out of the app's log: when the Board showed, and the main
/// thread's hangs.
final class LaunchPerfTests: PerfTestCase {
    override func setUp() async throws {
        try prepare(links: ["HERDER_PAIR_LINK", "HERDER_LOAD_LINK"])
    }

    /// Opening the app after time away: its second launch, on what the first left on disk,
    /// while the machines send what happened since.
    func testLaunch() {
        launch()
        // The first launch signs in and reads every session; the client saves what it has every
        // 30 s.
        Thread.sleep(forTimeInterval: 90)
        app.terminate()
        measured {
            launch()
            Thread.sleep(forTimeInterval: Self.aftermath)
        }
    }

    /// The same with no machine answering, as on a network that cannot reach them. perf.sh
    /// runs it apart, after `testLaunch` has left the profile and with the fake daemons
    /// stopped; a connection gives up after 10 s.
    func testLaunchUnreachable() {
        measured {
            launch()
            Thread.sleep(forTimeInterval: Self.aftermath)
        }
    }

    /// Opening the longest session, of 16,000 events.
    func testOpenLongSession() {
        launch()
        settle()
        let session = row("Load session 0 on big")
        reveal(session)
        measured {
            press(session)
            XCTAssertTrue(
                app.descendants(matching: .any)["transcript"].waitForExistence(timeout: 120),
                "the session did not open")
            Thread.sleep(forTimeInterval: 10)
        }
    }

    #if os(iOS)
    /// Coming back to the app after half a minute in the background, where it drops its
    /// connections.
    func testResume() {
        launch()
        settle()
        XCUIDevice.shared.press(.home)
        Thread.sleep(forTimeInterval: 30)
        measured {
            app.activate()
            XCTAssertTrue(loaded.waitForExistence(timeout: 120), "the Board did not come back")
            Thread.sleep(forTimeInterval: 10)
        }
    }
    #endif

    /// How long after the Board shows a launch is still measured: the machines' answers, or
    /// their silence, arrive after it.
    private static let aftermath: TimeInterval = 20

    /// A session of the loaded machine on the Board.
    private var loaded: XCUIElement {
        app.buttons.matching(NSPredicate(format: "label CONTAINS %@", ", Load session ")).firstMatch
    }

    /// Launches the app and waits for the Board to list the loaded machine's sessions.
    private func launch() {
        app.launch()
        XCTAssertTrue(loaded.waitForExistence(timeout: 180), "the Board did not load")
    }
}
