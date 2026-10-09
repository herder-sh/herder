import Testing
@testable import HerderKit

struct CloneFolderTests {
    @Test func aCloneGoesIntoAFolderNamedAfterTheRepository() {
        #expect(ProjectPicker.cloneFolder("herder-sh/herder") == "~/Projects/herder")
        #expect(ProjectPicker.cloneFolder("https://github.com/herder-sh/herder.git/") == "~/Projects/herder")
        #expect(ProjectPicker.cloneFolder("git@github.com:herder-sh/herder.git") == "~/Projects/herder")
        #expect(ProjectPicker.cloneFolder("  ") == "")
        #expect(ProjectPicker.cloneFolder("coingate/coingate") == "~/Projects/coingate")
        #expect(ProjectPicker.cloneFolder("  https://example.com/team/project.git  ") == "~/Projects/project")
    }

    @Test func changingMachinesDiscardsPathsAndErrorsFromThePreviousMachine() {
        var input = ProjectRepositoryInput()
        input.repo = "/old-machine/repo"
        input.url = "coingate/coingate"
        input.into = "/old-machine/clone"
        input.changingLocation = true
        input.addError = "exists already"
        input.changeMachine()
        #expect(input.repo.isEmpty)
        #expect(input.into == "~/Projects/coingate")
        #expect(!input.changingLocation)
        #expect(input.addError == nil)
    }

    @Test func recoveringFromACloneFailureUsesItsDestinationWithoutCloningAgain() {
        var input = ProjectRepositoryInput()
        input.cloning = true
        input.into = "~/Projects/coingate"
        input.addError = "exists already"
        input.useExistingRepository()
        #expect(!input.cloning)
        #expect(input.repo == "~/Projects/coingate")
        #expect(input.addError == nil)
    }

    @Test func suggestedLocationsFollowTheRepositoryUntilTheUserChangesThem() {
        var input = ProjectRepositoryInput()
        input.updateCloneDestination(from: "", to: "team/first")
        #expect(input.into == "~/Projects/first")
        input.updateCloneDestination(from: "team/first", to: "team/second")
        #expect(input.into == "~/Projects/second")
        input.into = "/custom/location"
        input.updateCloneDestination(from: "team/second", to: "team/third")
        #expect(input.into == "/custom/location")
    }

    @Test func aSuggestedLocationMovesToTheMachinesProjectsFolder() {
        var input = ProjectRepositoryInput()
        input.url = "team/app"
        input.updateCloneDestination(from: "", to: input.url)
        input.setProjectsDir("/srv/src/")
        #expect(input.into == "/srv/src/app")
        input.url = "team/lib"
        input.updateCloneDestination(from: "team/app", to: input.url)
        #expect(input.into == "/srv/src/lib")
        // A location the user chose stays.
        input.into = "/custom/lib"
        input.setProjectsDir("/home/dev/Projects")
        #expect(input.into == "/custom/lib")
    }
}
