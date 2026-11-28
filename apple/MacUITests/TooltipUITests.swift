import AppKit
import XCTest

/// Diagnostic: which of a session's buttons show their tooltip on hover.
@MainActor
final class TooltipUITests: XCTestCase {
    private static let idle = "Tidy the README badges"
    private var app: XCUIApplication!

    override func setUp() async throws {
        continueAfterFailure = true
        let environment = ProcessInfo.processInfo.environment
        let path = try XCTUnwrap(environment["HERDER_APP"], "needs HERDER_APP")
        app = XCUIApplication(url: URL(fileURLWithPath: path))
        app.launchEnvironment["HERDER_PROFILE"] = "ui-tests-\(UUID().uuidString)"
        app.launchEnvironment["HERDER_FAKE_FLEET"] = try XCTUnwrap(environment["HERDER_PAIR_LINK"], "needs HERDER_PAIR_LINK")
        app.launchEnvironment["HERDER_FAKE_PASSKEY"] = try XCTUnwrap(environment["HERDER_PASSKEY"], "needs HERDER_PASSKEY")
        app.launch()
    }

    override func tearDown() async throws {
        app?.terminate()
    }

    func testDiagnoseTooltips() throws {
        let row = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", ", \(Self.idle),")).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 30))
        Thread.sleep(forTimeInterval: 3)
        row.click()
        XCTAssertTrue(app.descendants(matching: .any)["transcript"].firstMatch.waitForExistence(timeout: 15))
        Thread.sleep(forTimeInterval: 2)
        for (label, kind) in [("Find in Transcript", "header"), ("Dictate", "mic"), ("Send", "send"),
                              ("Events and Statistics", "header2")] {
            let button = app.buttons[label].firstMatch
            XCTAssertTrue(button.waitForExistence(timeout: 5), "\(kind): no button \(label)")
            app.windows.firstMatch.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.4)).hover()
            Thread.sleep(forTimeInterval: 1)
            button.hover()
            let tag = app.helpTags.firstMatch
            let shown = tag.waitForExistence(timeout: 6)
            let shot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
            shot.name = "tooltip-\(kind)"
            shot.lifetime = .keepAlways
            add(shot)
            XCTAssertTrue(shown, "\(kind): no tooltip; tree: \(app.debugDescription.prefix(200))")
            if shown { print("TOOLTIP \(kind): \(tag.debugDescription)") }
        }
    }
}
