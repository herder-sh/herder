import Testing
@testable import HerderKit

struct CloneFolderTests {
    @Test func aCloneGoesIntoAFolderNamedAfterTheRepository() {
        #expect(ProjectPicker.cloneFolder("herder-sh/herder") == "~/src/herder")
        #expect(ProjectPicker.cloneFolder("https://github.com/herder-sh/herder.git/") == "~/src/herder")
        #expect(ProjectPicker.cloneFolder("git@github.com:herder-sh/herder.git") == "~/src/herder")
        #expect(ProjectPicker.cloneFolder("  ") == "")
    }
}
