@testable import HerderKit
import Testing

struct SessionHeadingTests {
    @Test func theCaptionNamesTheProjectThenTheMachine() {
        #expect(SessionHeading.detail(project: "herder", machine: "studio", archiving: false) == "herder · studio")
    }

    @Test func theCaptionSkipsAMissingOrEmptyProject() {
        #expect(SessionHeading.detail(project: "", machine: "studio", archiving: false) == "studio")
        #expect(SessionHeading.detail(project: nil, machine: "studio", archiving: false) == "studio")
        #expect(SessionHeading.detail(project: nil, machine: nil, archiving: false) == "")
    }

    @Test func theCaptionSaysWhileTheSessionIsBeingArchived() {
        #expect(SessionHeading.detail(project: "herder", machine: "studio", archiving: true) == "Archiving…")
    }
}
