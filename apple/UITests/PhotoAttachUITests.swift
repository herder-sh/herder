import XCTest

/// Attaches a photo from the library to a prompt: against the fake daemons of `PairingUITests`,
/// in the session of `fake-host-1` waiting on an approval, where the prompt queues. The
/// simulator's library has photos of its own.
final class PhotoAttachUITests: XCTestCase {
    @MainActor
    func testAPhotoPickedFromTheLibraryGoesWithThePrompt() throws {
        let app = XCUIApplication()
        app.launch()
        try app.pairUnlessPaired()

        let session = app.descendants(matching: .any)["request-session"].firstMatch
        XCTAssertTrue(session.waitForExistence(timeout: 15))
        session.tap()
        let attach = app.descendants(matching: .any)["attach"].firstMatch
        XCTAssertTrue(attach.waitForExistence(timeout: 10))
        attach.tap()
        let library = app.buttons["Photo Library"]
        XCTAssertTrue(library.waitForExistence(timeout: 5))
        library.tap()

        // The picker runs out of process; its photos show as images in the app's hierarchy. On a
        // simulator's first use of its library they take a minute to load.
        let photo = app.images.matching(identifier: "PXGGridLayout-Info").firstMatch
        XCTAssertTrue(photo.waitForExistence(timeout: 120), "the photo library did not open")
        photo.tap()
        app.buttons["Done"].tap()

        let strip = app.descendants(matching: .any)["attachment-strip"].firstMatch
        XCTAssertTrue(strip.waitForExistence(timeout: 15), "the picked photo was not attached")
        app.buttons["Send"].tap()
        XCTAssertTrue(strip.waitForNonExistence(timeout: 10), "the photo did not go with the prompt")
        // The prompt, its photo's marker, queues behind the approval on the machine.
        let queued = app.staticTexts["[Image #1]"]
        XCTAssertTrue(queued.waitForExistence(timeout: 10), "the machine did not queue the prompt")

        // Off the queue again, leaving the session as the next tests expect it.
        queued.swipeLeft()
        app.buttons["Remove"].tap()
        XCTAssertTrue(queued.waitForNonExistence(timeout: 10))
    }
}
