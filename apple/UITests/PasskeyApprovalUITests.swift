import XCTest

/// Approves a new machine with a passkey: against `fake_daemon --own`, which waits for an owner
/// as after `herder claim` (pass the link it prints as `TEST_RUNNER_HERDER_OWN_LINK`), opens its
/// `herder://own` link, checks the code and the machine's name, approves with the app's software
/// passkey (`-softwarePasskey`) and lands on the machine in Fleet.
final class PasskeyApprovalUITests: XCTestCase {
    @MainActor
    func testApprovingAMachineWithAPasskeyOpensIt() throws {
        let link = try XCTUnwrap(ProcessInfo.processInfo.environment["HERDER_OWN_LINK"], "needs HERDER_OWN_LINK")
        let code = try XCTUnwrap(URLComponents(string: link)?.queryItems?.first { $0.name == "code" }?.value)
        let app = XCUIApplication()
        app.launchArguments = ["-softwarePasskey"]
        try app.launchOnFakeFleet()

        app.open(try XCTUnwrap(URL(string: link)))

        XCTAssertTrue(app.staticTexts["fake-own wants you as its owner"].waitForExistence(timeout: 10))
        // The code in two halves, as the terminal prints it: "KSJ-B18" reads "KSJ · B18".
        let halves = code.split(separator: "-").joined(separator: " · ")
        XCTAssertEqual(app.staticTexts["approval-code"].label, halves)
        app.buttons["approve"].tap()

        XCTAssertTrue(app.staticTexts["fake-own is yours"].waitForExistence(timeout: 30), "the machine did not take the passkey")
        app.buttons["Open fake-own"].tap()

        XCTAssertTrue(app.tabBars.buttons["Fleet"].isSelected, "Open did not go to Fleet")
        XCTAssertTrue(app.textFields["machine-name"].waitForExistence(timeout: 15), "Fleet did not open the machine")
        XCTAssertEqual(app.textFields["machine-name"].placeholderValue, "fake-own")
    }
}
