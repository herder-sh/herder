import XCTest

/// Approves a new machine from its `herder://own` link (`fake_daemon --own` prints it; pass it to
/// `xcodebuild test` as `TEST_RUNNER_HERDER_OWN_LINK`), with the software passkey in Face ID's
/// place: the code to compare, Approve, and the machine is yours.
final class ApproveMachineUITests: XCTestCase {
    @MainActor
    func testApprovingANewMachineMakesItYours() throws {
        let link = try XCTUnwrap(ProcessInfo.processInfo.environment["HERDER_OWN_LINK"], "needs HERDER_OWN_LINK")
        let app = XCUIApplication()
        // A profile of its own, paired with nothing, and a passkey that needs no Face ID.
        app.launchEnvironment["HERDER_PROFILE"] = "approve-\(UUID().uuidString)"
        app.launchEnvironment["HERDER_SOFTWARE_PASSKEY"] = "1"
        app.launchForTests()
        XCTAssertTrue(app.tabBars.buttons["Board"].waitForExistence(timeout: 30), "the app did not show the Board")

        app.open(try XCTUnwrap(URL(string: link)))
        XCTAssertTrue(app.staticTexts["new-server wants you as its owner"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.staticTexts["approval-code"].exists, "no code to compare with the terminal's")
        app.buttons["approve"].tap()

        XCTAssertTrue(app.staticTexts["new-server is yours"].waitForExistence(timeout: 30))
        app.buttons["Open new-server"].tap()
        XCTAssertTrue(app.staticTexts["new-server"].firstMatch.waitForExistence(timeout: 15),
                      "the new machine's page did not open")
    }
}
