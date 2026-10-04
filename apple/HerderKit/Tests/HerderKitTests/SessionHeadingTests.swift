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

    @Test func theMenuIsTitledWithTheBranchAndTheForkOrigin() {
        let origin = ForkOrigin(sessionId: "01A", hostId: "host-a")
        #expect(SessionHeading.facts(branch: "p7-header", forkedFrom: nil) == "p7-header")
        #expect(SessionHeading.facts(branch: "p7-header", forkedFrom: origin) == "p7-header\nForked from 01A on host-a")
        #expect(SessionHeading.facts(branch: nil, forkedFrom: origin) == "Forked from 01A on host-a")
        #expect(SessionHeading.facts(branch: nil, forkedFrom: nil) == "")
    }
}
