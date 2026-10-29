import XCTest

/// A tap on a task row opens its session, on the Board, in a project and in Chats, and a swipe
/// archives it. Against the fake daemons of `FakeFleetUITests`, pairing first unless an earlier
/// test did; it starts the chat it taps, so it runs after `TabsUITests`, which wants none.
final class TaskRowUITests: XCTestCase {
    @MainActor
    func testTappingATaskRowOpensItsSessionAndSwipingArchivesIt() throws {
        let app = XCUIApplication()
        try app.launchOnFakeFleet()

        // The Board: the session waiting on an approval.
        let waiting = app.taskRow(NSPredicate(format: "label BEGINSWITH 'Waiting for you, '"))
        XCTAssertTrue(waiting.waitForExistence(timeout: 15))
        app.opens(waiting)

        // Its project, a repository with no project yet.
        app.tabBars.buttons["Projects"].tap()
        let project = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'No project yet, '")).firstMatch
        XCTAssertTrue(project.waitForExistence(timeout: 10))
        project.tap()
        let inProject = app.taskRow(NSPredicate(format: "label BEGINSWITH 'Waiting for you, '"))
        XCTAssertTrue(inProject.waitForExistence(timeout: 10))
        app.opens(inProject)

        // A chat, started here.
        app.tabBars.buttons["Chats"].tap()
        app.navigationBars["Chats"].buttons["New Chat"].tap()
        let composer = app.textViews["composer"].firstMatch
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        composer.tap()
        // The prompt the fake adapter's script expects.
        composer.typeText("Run the tests.")
        app.buttons["Send"].tap()
        XCTAssertTrue(app.message("Run the tests.").waitForExistence(timeout: 15))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let chat = app.taskRow(NSPredicate(format: "label MATCHES '(Working|Waiting for you|Idle), .*'"))
        XCTAssertTrue(chat.waitForExistence(timeout: 10))
        app.opens(chat)

        // A swipe archives it.
        chat.swipeLeft()
        app.buttons["Archive"].tap()
        XCTAssertTrue(app.staticTexts["Archived · 1"].waitForExistence(timeout: 10), "the swipe did not archive the chat")
    }
}

private extension XCUIApplication {
    /// The first task row whose label matches: the button that opens its session.
    func taskRow(_ predicate: NSPredicate) -> XCUIElement {
        buttons.matching(predicate).firstMatch
    }

    /// Taps `row`, checks its session opened, and goes back to the list.
    @MainActor
    func opens(_ row: XCUIElement, file: StaticString = #filePath, line: UInt = #line) {
        row.tap()
        XCTAssertTrue(descendants(matching: .any)["composer"].waitForExistence(timeout: 5),
                      "the row's session did not open", file: file, line: line)
        navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(row.waitForExistence(timeout: 5), file: file, line: line)
    }
}
